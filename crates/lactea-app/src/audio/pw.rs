//! Native PipeWire capture of "what you hear": a capture stream with `stream.capture.sink=true`
//! that the session manager links to the **default sink's monitor** and re-links when the
//! default sink changes.
//!
//! A registry/metadata watcher on the same loop logs which node the stream is actually linked
//! to and tracks the `default.audio.sink` metadata, so the log always says what is being
//! listened to (and the title/monitor can distinguish "nothing playing" from "not capturing").

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use pipewire as pw;
use pw::metadata::{Metadata, MetadataListener};
use pw::properties::properties;
use pw::registry::{GlobalObject, RegistryRc};
use pw::spa;
use pw::spa::pod::Pod;
use pw::types::ObjectType;
use spa::param::format::{MediaSubtype, MediaType};
use spa::param::format_utils;

use super::{AudioSource, CHANNELS, RING_FRAMES, SourceShared, drain_ring, push_frames, to_stereo};

// spa_audio_channel: UNKNOWN=0, NA=1, MONO=2, FL=3, FR=4.
const CH_FL: u32 = 3;
const CH_FR: u32 = 4;

struct Quit;

#[derive(Default)]
struct PwInfo {
    /// Description of the node(s) our stream is linked to.
    node: Mutex<String>,
    state: Mutex<String>,
}

pub struct PipewireCapture {
    consumer: rtrb::Consumer<f32>,
    shared: Arc<SourceShared>,
    info: Arc<PwInfo>,
    quit: pw::channel::Sender<Quit>,
    thread: Option<JoinHandle<()>>,
    rate: u32,
}

#[derive(Debug, Clone)]
struct NodeInfo {
    name: String,
    desc: String,
    class: String,
}

/// Registry mirror: enough of the graph to say what our stream is connected to.
#[derive(Default)]
struct Graph {
    nodes: HashMap<u32, NodeInfo>,
    /// link id -> (output node, input node)
    links: HashMap<u32, (u32, u32)>,
    default_sink: Option<String>,
    our_node: Option<u32>,
    reported: String,
}

impl Graph {
    fn refresh(&mut self, info: &PwInfo) {
        let Some(our) = self.our_node else { return };
        // One audio link per channel: collapse to the distinct source nodes.
        let mut outs: Vec<u32> = self
            .links
            .values()
            .filter(|(_, i)| *i == our)
            .map(|(o, _)| *o)
            .collect();
        outs.sort_unstable();
        outs.dedup();
        let srcs: Vec<String> = outs
            .iter()
            .map(|o| match self.nodes.get(o) {
                Some(n) => format!("'{}' ({}, node {o})", n.desc, n.name),
                None => format!("node {o}"),
            })
            .collect();
        let text = if srcs.is_empty() {
            "not linked yet".to_string()
        } else {
            srcs.join(", ")
        };
        let key = format!("{text} | default sink {:?}", self.default_sink);
        if key != self.reported {
            log::info!(
                "pipewire: capture stream node {our} <- {text}; default sink = {}",
                self.default_sink.as_deref().unwrap_or("(unknown)")
            );
            self.reported = key;
        }
        *info.node.lock() = text;
    }
}

/// `{"name":"alsa_output.pci-...analog-stereo"}` -> the name. (Tiny, tolerant extraction; the
/// metadata value is JSON but only this one field matters.)
fn json_name(v: &str) -> Option<String> {
    let i = v.find("\"name\"")?;
    let rest = &v[i + 6..];
    let rest = rest[rest.find(':')? + 1..].trim_start();
    let rest = rest.strip_prefix('"')?;
    Some(rest[..rest.find('"')?].to_string())
}

struct StreamData {
    channels: usize,
    rate: u32,
    producer: Rc<RefCell<rtrb::Producer<f32>>>,
    shared: Arc<SourceShared>,
    info: Arc<PwInfo>,
    graph: Rc<RefCell<Graph>>,
    scratch: Vec<f32>,
    stereo: Vec<f32>,
}

fn format_params(rate: u32) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut audio_info = spa::param::audio::AudioInfoRaw::new();
    audio_info.set_format(spa::param::audio::AudioFormat::F32LE);
    audio_info.set_rate(rate);
    audio_info.set_channels(CHANNELS as u32);
    let mut position = [0u32; spa::sys::SPA_AUDIO_MAX_CHANNELS as usize];
    position[0] = CH_FL;
    position[1] = CH_FR;
    audio_info.set_position(position);
    let obj = spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: audio_info.into(),
    };
    Ok(spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )?
    .0
    .into_inner())
}

/// One connection to the PipeWire daemon: stream + registry watcher. Returns when the
/// connection is lost or the loop is asked to quit.
#[allow(clippy::too_many_arguments)]
fn session(
    mainloop: &pw::main_loop::MainLoopRc,
    context: &pw::context::ContextRc,
    rate: u32,
    target: Option<&str>,
    producer: &Rc<RefCell<rtrb::Producer<f32>>>,
    shared: &Arc<SourceShared>,
    info: &Arc<PwInfo>,
    ready: &mut Option<mpsc::Sender<Result<(), String>>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let core = context.connect_rc(None)?;
    let lost = Rc::new(Cell::new(false));
    let _core_listener = core
        .add_listener_local()
        .error({
            let (ml, lost) = (mainloop.clone(), lost.clone());
            let shared = shared.clone();
            move |id, _seq, res, msg| {
                log::error!("pipewire core error (object {id}): {msg} (res {res})");
                // -EPIPE on the core object: the daemon went away.
                if id == 0 && res == -32 {
                    lost.set(true);
                    shared.set_failure(Some("PipeWire connection lost".into()));
                    ml.quit();
                }
            }
        })
        .register();

    let graph = Rc::new(RefCell::new(Graph::default()));
    let registry: RegistryRc = core.get_registry_rc()?;
    let reg_weak = registry.downgrade();
    let metas: Rc<RefCell<Vec<(Metadata, MetadataListener)>>> = Rc::new(RefCell::new(Vec::new()));
    let _registry_listener = registry
        .add_listener_local()
        .global({
            let (graph, info, metas) = (graph.clone(), info.clone(), metas.clone());
            move |global: &GlobalObject<&spa::utils::dict::DictRef>| {
                let Some(props) = global.props else { return };
                match global.type_ {
                    ObjectType::Node => {
                        let name = props.get("node.name").unwrap_or("").to_string();
                        let desc = props
                            .get("node.description")
                            .or_else(|| props.get("node.nick"))
                            .unwrap_or(&name)
                            .to_string();
                        let class = props.get("media.class").unwrap_or("").to_string();
                        let mut g = graph.borrow_mut();
                        g.nodes.insert(global.id, NodeInfo { name, desc, class });
                        g.refresh(&info);
                    }
                    ObjectType::Link => {
                        let parse = |k| props.get(k).and_then(|v| v.parse::<u32>().ok());
                        if let (Some(o), Some(i)) =
                            (parse("link.output.node"), parse("link.input.node"))
                        {
                            let mut g = graph.borrow_mut();
                            g.links.insert(global.id, (o, i));
                            g.refresh(&info);
                        }
                    }
                    ObjectType::Metadata if props.get("metadata.name") == Some("default") => {
                        let Some(registry) = reg_weak.upgrade() else {
                            return;
                        };
                        let Ok(meta) = registry.bind::<Metadata, _>(global) else {
                            return;
                        };
                        let listener = meta
                            .add_listener_local()
                            .property({
                                let (graph, info) = (graph.clone(), info.clone());
                                move |_subject, key, _type, value| {
                                    if key == Some("default.audio.sink") {
                                        let mut g = graph.borrow_mut();
                                        g.default_sink = value.and_then(json_name);
                                        g.refresh(&info);
                                    }
                                    0
                                }
                            })
                            .register();
                        metas.borrow_mut().push((meta, listener));
                    }
                    _ => {}
                }
            }
        })
        .global_remove({
            let (graph, info) = (graph.clone(), info.clone());
            move |id| {
                let mut g = graph.borrow_mut();
                g.nodes.remove(&id);
                g.links.remove(&id);
                g.refresh(&info);
            }
        })
        .register();

    let mut props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Music",
        *pw::keys::NODE_NAME => "lactea-capture",
        *pw::keys::APP_NAME => "lactea",
        *pw::keys::NODE_DESCRIPTION => "lactea visualizer capture",
        // Link to the default sink's monitor (and follow it) instead of the default source.
        *pw::keys::STREAM_CAPTURE_SINK => "true",
    };
    props.insert(*pw::keys::NODE_LATENCY, format!("1024/{rate}"));
    if let Some(t) = target {
        props.insert("target.object", t);
    }
    let stream = pw::stream::StreamBox::new(&core, "lactea-capture", props)?;

    let data = StreamData {
        channels: CHANNELS,
        rate,
        producer: producer.clone(),
        shared: shared.clone(),
        info: info.clone(),
        graph: graph.clone(),
        scratch: Vec::with_capacity(8192),
        stereo: Vec::with_capacity(8192),
    };
    let _listener = stream
        .add_local_listener_with_user_data(data)
        .state_changed(|stream, ud, old, new| {
            log::info!("pipewire stream: {old:?} -> {new:?}");
            *ud.info.state.lock() = format!("{new:?}");
            match &new {
                pw::stream::StreamState::Error(e) => {
                    ud.shared.set_failure(Some(format!("PipeWire stream error: {e}")));
                }
                pw::stream::StreamState::Streaming => {
                    ud.shared.set_failure(None);
                    ud.shared.set_detail("streaming");
                }
                other => ud.shared.set_detail(format!("stream {other:?}")),
            }
            if !matches!(new, pw::stream::StreamState::Unconnected | pw::stream::StreamState::Connecting) {
                let id = stream.node_id();
                let mut g = ud.graph.borrow_mut();
                if g.our_node != Some(id) {
                    g.our_node = Some(id);
                }
                g.refresh(&ud.info);
            }
        })
        .param_changed(|_, ud, id, param| {
            let Some(param) = param else { return };
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            let Ok((mt, mst)) = format_utils::parse_format(param) else {
                return;
            };
            if mt != MediaType::Audio || mst != MediaSubtype::Raw {
                return;
            }
            let mut fmt = spa::param::audio::AudioInfoRaw::default();
            if fmt.parse(param).is_ok() {
                ud.channels = (fmt.channels() as usize).max(1);
                log::info!("pipewire capture format: {} Hz, {} ch", fmt.rate(), fmt.channels());
                if fmt.rate() != ud.rate {
                    log::warn!(
                        "pipewire negotiated {} Hz, requested {} Hz: visualizer timing is off by {:.1}%",
                        fmt.rate(),
                        ud.rate,
                        (fmt.rate() as f64 / ud.rate as f64 - 1.0) * 100.0
                    );
                }
            }
        })
        .process(|stream, ud| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let datas = buffer.datas_mut();
            let Some(d) = datas.first_mut() else { return };
            let (offset, size) = (d.chunk().offset() as usize, d.chunk().size() as usize);
            let Some(bytes) = d.data() else { return };
            let end = (offset + size).min(bytes.len());
            let bytes = &bytes[offset.min(end)..end];
            ud.scratch.clear();
            ud.scratch.extend(
                bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|b| f32::from_le_bytes(*b)),
            );
            ud.stereo.clear();
            to_stereo(&ud.scratch, ud.channels, &mut ud.stereo);
            push_frames(&mut ud.producer.borrow_mut(), &ud.stereo, &ud.shared);
        })
        .register()?;

    let values = format_params(rate)?;
    let mut params = [Pod::from_bytes(&values).ok_or("format pod is malformed")?];
    stream.connect(
        spa::utils::Direction::Input,
        None,
        pw::stream::StreamFlags::AUTOCONNECT
            | pw::stream::StreamFlags::MAP_BUFFERS
            | pw::stream::StreamFlags::RT_PROCESS,
        &mut params,
    )?;
    if let Some(r) = ready.take() {
        let _ = r.send(Ok(()));
    }
    mainloop.run();
    let _ = stream.disconnect();
    Ok(())
}

fn thread_main(
    rate: u32,
    target: Option<String>,
    producer: rtrb::Producer<f32>,
    shared: Arc<SourceShared>,
    info: Arc<PwInfo>,
    quit_rx: pw::channel::Receiver<Quit>,
    ready: mpsc::Sender<Result<(), String>>,
) {
    pw::init();
    let setup = || -> Result<(pw::main_loop::MainLoopRc, pw::context::ContextRc), pw::Error> {
        let mainloop = pw::main_loop::MainLoopRc::new(None)?;
        let context = pw::context::ContextRc::new(&mainloop, None)?;
        Ok((mainloop, context))
    };
    let (mainloop, context) = match setup() {
        Ok(v) => v,
        Err(e) => {
            let _ = ready.send(Err(format!("PipeWire init failed: {e}")));
            return;
        }
    };
    let quit = Rc::new(Cell::new(false));
    let _rx = quit_rx.attach(mainloop.loop_(), {
        let (ml, quit) = (mainloop.clone(), quit.clone());
        move |_| {
            quit.set(true);
            ml.quit();
        }
    });
    let producer = Rc::new(RefCell::new(producer));
    let mut ready = Some(ready);
    loop {
        if quit.get() {
            break;
        }
        match session(
            &mainloop,
            &context,
            rate,
            target.as_deref(),
            &producer,
            &shared,
            &info,
            &mut ready,
        ) {
            Ok(()) => {}
            Err(e) => {
                let msg = format!("cannot connect to PipeWire: {e}");
                if let Some(r) = ready.take() {
                    let _ = r.send(Err(msg));
                    return;
                }
                log::warn!("{msg}; retrying");
                shared.set_failure(Some(msg));
            }
        }
        if quit.get() {
            break;
        }
        // Connection lost: wait (still serving the quit channel) and reconnect.
        log::warn!("pipewire: connection ended, reconnecting in 1 s");
        let until = Instant::now() + Duration::from_secs(1);
        while Instant::now() < until && !quit.get() {
            mainloop
                .loop_()
                .iterate(pw::loop_::Timeout::Finite(Duration::from_millis(100)));
        }
    }
}

impl PipewireCapture {
    /// Start capturing the default sink's monitor at `rate` Hz (stereo). `target` optionally
    /// names a specific node (`node.name` or object serial) instead of following the default.
    pub fn start(rate: u32, target: Option<String>) -> Result<Self, String> {
        let (producer, consumer) = rtrb::RingBuffer::<f32>::new(RING_FRAMES * CHANNELS);
        let shared = Arc::new(SourceShared::default());
        let info = Arc::new(PwInfo::default());
        let (quit_tx, quit_rx) = pw::channel::channel::<Quit>();
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread = {
            let (shared, info) = (shared.clone(), info.clone());
            std::thread::Builder::new()
                .name("lactea-pipewire".into())
                .spawn(move || thread_main(rate, target, producer, shared, info, quit_rx, ready_tx))
                .map_err(|e| format!("cannot spawn PipeWire thread: {e}"))?
        };
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                let _ = thread.join();
                return Err(e);
            }
            Err(_) => {
                let _ = quit_tx.send(Quit);
                return Err("PipeWire did not respond within 5 s".into());
            }
        }
        Ok(Self {
            consumer,
            shared,
            info,
            quit: quit_tx,
            thread: Some(thread),
            rate,
        })
    }
}

impl AudioSource for PipewireCapture {
    fn describe(&self) -> String {
        let node = self.info.node.lock().clone();
        let state = self.info.state.lock().clone();
        if node.is_empty() {
            format!(
                "PipeWire default-sink monitor ({})",
                if state.is_empty() {
                    "connecting"
                } else {
                    &state
                }
            )
        } else {
            format!("PipeWire monitor: {node}")
        }
    }

    fn sample_rate(&self) -> u32 {
        self.rate
    }

    fn poll(&mut self, _now: Instant, out: &mut Vec<f32>) {
        drain_ring(&mut self.consumer, out);
    }

    fn shared(&self) -> &SourceShared {
        &self.shared
    }
}

impl Drop for PipewireCapture {
    fn drop(&mut self) {
        let _ = self.quit.send(Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// One line per audio sink for `--list-devices`, `*` marking the default.
pub fn list_sinks() -> Result<Vec<String>, String> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(|e| e.to_string())?;
    let context = pw::context::ContextRc::new(&mainloop, None).map_err(|e| e.to_string())?;
    let core = context
        .connect_rc(None)
        .map_err(|e| format!("cannot connect to PipeWire: {e}"))?;
    let registry = core.get_registry_rc().map_err(|e| e.to_string())?;
    let nodes: Rc<RefCell<Vec<(u32, NodeInfo)>>> = Rc::default();
    let default_sink: Rc<RefCell<Option<String>>> = Rc::default();
    let metas: Rc<RefCell<Vec<(Metadata, MetadataListener)>>> = Rc::default();
    let weak = registry.downgrade();
    let _l = registry
        .add_listener_local()
        .global({
            let (nodes, default_sink, metas) = (nodes.clone(), default_sink.clone(), metas.clone());
            move |g: &GlobalObject<&spa::utils::dict::DictRef>| {
                let Some(props) = g.props else { return };
                match g.type_ {
                    ObjectType::Node => {
                        let class = props.get("media.class").unwrap_or("");
                        if class == "Audio/Sink" {
                            let name = props.get("node.name").unwrap_or("").to_string();
                            let desc = props.get("node.description").unwrap_or(&name).to_string();
                            nodes.borrow_mut().push((
                                g.id,
                                NodeInfo {
                                    name,
                                    desc,
                                    class: class.to_string(),
                                },
                            ));
                        }
                    }
                    ObjectType::Metadata if props.get("metadata.name") == Some("default") => {
                        if let Some(reg) = weak.upgrade()
                            && let Ok(meta) = reg.bind::<Metadata, _>(g)
                        {
                            let l = meta
                                .add_listener_local()
                                .property({
                                    let ds = default_sink.clone();
                                    move |_, key, _, value| {
                                        if key == Some("default.audio.sink") {
                                            *ds.borrow_mut() = value.and_then(json_name);
                                        }
                                        0
                                    }
                                })
                                .register();
                            metas.borrow_mut().push((meta, l));
                        }
                    }
                    _ => {}
                }
            }
        })
        .register();
    // Two round trips: registry burst, then the metadata properties of what we bound.
    let pending = Rc::new(Cell::new(2u32));
    let _core_l = core
        .add_listener_local()
        .done({
            let (ml, pending) = (mainloop.clone(), pending.clone());
            move |_, _| {
                pending.set(pending.get().saturating_sub(1));
                if pending.get() == 0 {
                    ml.quit();
                }
            }
        })
        .register();
    let _ = core.sync(0);
    let _ = core.sync(1);
    // Safety net: never hang.
    let ml = mainloop.clone();
    let timer = mainloop.loop_().add_timer(move |_| ml.quit());
    timer.update_timer(Some(Duration::from_secs(3)), None);
    mainloop.run();
    let default = default_sink.borrow().clone();
    let mut out: Vec<String> = nodes
        .borrow()
        .iter()
        .map(|(id, n)| {
            let star = if default.as_deref() == Some(n.name.as_str()) {
                "*"
            } else {
                " "
            };
            format!(" {star} {} ({}, node {id}) [{}]", n.desc, n.name, n.class)
        })
        .collect();
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_default_sink_name() {
        assert_eq!(
            json_name(r#"{"name":"alsa_output.pci-0000_00_1f.3.analog-stereo"}"#).as_deref(),
            Some("alsa_output.pci-0000_00_1f.3.analog-stereo")
        );
        assert_eq!(json_name(r#"{ "name" : "x" }"#).as_deref(), Some("x"));
        assert_eq!(json_name("{}"), None);
    }
}
