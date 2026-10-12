//! Vulkan interop: run the engine on the host's own `VkDevice` and render into host `VkImage`s.
//!
//! wgpu cannot adopt a foreign image on a device of its own, so the engine has to *be* on the host's
//! device. The host keeps creating the `VkInstance` and `VkDevice` itself; lactea only wraps the
//! handles through `wgpu::hal::vulkan` and never destroys them. The contract, in order:
//!
//! 1. Create the `VkInstance` with `apiVersion` >= 1.1 *and* >= the version the physical device
//!    reports (`vkEnumerateInstanceVersion` is always safe: wgpu assumes the device exposes every
//!    core version it reports, and a lower instance version hides those functions; lactea checks
//!    this and refuses), plus the extension `VK_KHR_get_physical_device_properties2` (wgpu assumes
//!    it is on whenever it exists).
//! 2. Fill a `LacteaVulkanHost` (instance, physical device, `vkGetInstanceProcAddr`, queue family)
//!    and call `lactea_vulkan_requirements`: it lists the device extensions wgpu needs and a
//!    `VkPhysicalDeviceFeatures` + `pNext` feature chain. Create the `VkDevice` with them (add your
//!    own extensions/features to the lists; chain your own structs in front of `device_pnext`).
//! 3. `lactea_create_vulkan` with the same host struct plus the created `device` and the extensions
//!    you really enabled.
//! 4. Every frame `lactea_render_to_vk_image` with an image of the engine's size, format
//!    `R8G8B8A8_UNORM` or `B8G8R8A8_UNORM` (the one named in the host struct) and usage
//!    `COLOR_ATTACHMENT`.
//!
//! Layout and synchronisation: lactea submits to the host's queue (`queue_family_index` /
//! `queue_index`, externally synchronised like any queue: do not submit to it from another thread
//! during the call). The image may be in any layout and its contents are discarded; it is left in
//! `VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL`, with the colour writes finished as soon as the
//! submission has executed. On the same queue a pipeline barrier (`COLOR_ATTACHMENT_OUTPUT` ->
//! whatever you do next) is enough; with `wait = true` the call blocks until the GPU is done, so any
//! queue can use the image afterwards. Destroy the engine before the `VkDevice`.
//!
//! Where wgpu has no Vulkan backend (Apple) the functions exist and return `LACTEA_STATUS_GPU`.

use std::ffi::{c_char, c_void};

use crate::{LacteaConfig, LacteaEngine, LacteaStatus, fail, guard};

const VK_FORMAT_R8G8B8A8_UNORM: u32 = 37;
const VK_FORMAT_B8G8R8A8_UNORM: u32 = 44;

/// The host's Vulkan objects. Dispatchable handles are passed as pointers, strings as arrays of
/// NUL-terminated names exactly as given to `vkCreateInstance` / `vkCreateDevice`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LacteaVulkanHost {
    /// `vkGetInstanceProcAddr` of the loader the host uses (cast the function pointer to `void *`).
    pub get_instance_proc_addr: *const c_void,
    /// `VkInstance`.
    pub instance: *mut c_void,
    /// `VkPhysicalDevice` the device was (or will be) created on.
    pub physical_device: *mut c_void,
    /// `VkDevice`. Ignored by `lactea_vulkan_requirements`.
    pub device: *mut c_void,
    /// Queue family and index lactea submits to (a family with graphics support; the queue must have
    /// been requested in `vkCreateDevice`). Ignored by `lactea_vulkan_requirements`.
    pub queue_family_index: u32,
    pub queue_index: u32,
    /// `VkApplicationInfo::apiVersion` the instance was created with (0 = 1.0).
    pub api_version: u32,
    /// Instance extensions enabled by the host.
    pub instance_extensions: *const *const c_char,
    pub instance_extension_count: u32,
    /// Device extensions enabled by the host. Ignored by `lactea_vulkan_requirements`.
    pub device_extensions: *const *const c_char,
    pub device_extension_count: u32,
    /// `VkFormat` of the images given to `lactea_render_to_vk_image`:
    /// `VK_FORMAT_R8G8B8A8_UNORM` (37) or `VK_FORMAT_B8G8R8A8_UNORM` (44). Ignored by
    /// `lactea_vulkan_requirements`.
    pub image_format: u32,
}

/// What `vkCreateDevice` needs for lactea. Free with `lactea_vulkan_requirements_free`; all
/// pointers stay valid until then.
#[repr(C)]
#[derive(Debug)]
pub struct LacteaVulkanRequirements {
    /// Device extension names to enable (a subset may already be on your list: duplicates are fine to
    /// deduplicate on your side, Vulkan forbids listing one twice).
    pub device_extensions: *const *const c_char,
    pub device_extension_count: u32,
    /// `const VkPhysicalDeviceFeatures *` for `VkDeviceCreateInfo::pEnabledFeatures`.
    pub enabled_features: *const c_void,
    /// `const void *` feature-struct chain (`VkPhysicalDeviceTimelineSemaphoreFeatures`, ...) for
    /// `VkDeviceCreateInfo::pNext`; NULL when empty. Do not use `enabled_features` together with a
    /// `VkPhysicalDeviceFeatures2` of your own.
    pub device_pnext: *const c_void,
    /// Owner of the memory above.
    pub private_data: *mut c_void,
}

/// Engine state of `lactea_create_vulkan` engines.
pub(crate) struct VulkanState {
    /// Format of the images `lactea_render_to_vk_image` accepts.
    pub format: wgpu::TextureFormat,
}

fn format_from_vk(vk_format: u32) -> Option<wgpu::TextureFormat> {
    match vk_format {
        VK_FORMAT_R8G8B8A8_UNORM => Some(wgpu::TextureFormat::Rgba8Unorm),
        VK_FORMAT_B8G8R8A8_UNORM => Some(wgpu::TextureFormat::Bgra8Unorm),
        _ => None,
    }
}

/// Which device extensions and features wgpu needs on `host`'s physical device. Fills `*out`; free
/// the result with `lactea_vulkan_requirements_free`. See the module documentation for the
/// sequence; only `get_instance_proc_addr`, `instance`, `physical_device`, `api_version` and the
/// instance extensions of `host` are read.
///
/// # Safety
/// `host` and `out` valid; the handles in `host` valid Vulkan objects; the name arrays readable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_vulkan_requirements(
    host: *const LacteaVulkanHost,
    out: *mut *mut LacteaVulkanRequirements,
) -> LacteaStatus {
    guard(|| {
        let (Some(host), false) = (unsafe { host.as_ref() }, out.is_null()) else {
            return fail(LacteaStatus::InvalidArgument, "host or out is NULL");
        };
        match imp::requirements(host) {
            Ok(r) => {
                unsafe { out.write(Box::into_raw(r)) };
                LacteaStatus::Ok
            }
            Err(e) => fail(LacteaStatus::Gpu, e),
        }
    })
}

/// Release a `lactea_vulkan_requirements` result. NULL is a no-op.
///
/// # Safety
/// `req` NULL or from `lactea_vulkan_requirements`, not used afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_vulkan_requirements_free(req: *mut LacteaVulkanRequirements) {
    if !req.is_null() {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            imp::free_requirements(&*unsafe { Box::from_raw(req) })
        }));
    }
}

/// Create an engine on the host's `VkDevice` (see the module documentation). `config` may be NULL
/// for defaults; its `width`/`height` are the size of the images later given to
/// `lactea_render_to_vk_image`. The engine has no internal target: use only
/// `lactea_render_to_vk_image` to render. Returns NULL on failure (`lactea_last_error`).
/// Everything else of the API (presets, PCM, switch settings, callbacks, resize) works as usual.
///
/// # Safety
/// `config` NULL or valid; `host` valid with all of its handles and arrays valid Vulkan objects; the
/// `VkDevice` created with at least the extensions of `lactea_vulkan_requirements` and its
/// features, and alive until `lactea_destroy` returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_create_vulkan(
    config: *const LacteaConfig,
    host: *const LacteaVulkanHost,
) -> *mut LacteaEngine {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let Some(host) = (unsafe { host.as_ref() }) else {
            crate::set_error("host is NULL");
            return std::ptr::null_mut();
        };
        let Some(format) = format_from_vk(host.image_format) else {
            crate::set_error(format!(
                "image_format {} is not supported (use VK_FORMAT_R8G8B8A8_UNORM = 37 or VK_FORMAT_B8G8R8A8_UNORM = 44)",
                host.image_format
            ));
            return std::ptr::null_mut();
        };
        let Some(mut cfg) = (unsafe { crate::engine_config(config) }) else {
            return std::ptr::null_mut();
        };
        cfg.output_format = format;
        match unsafe { imp::open_device(host) } {
            Ok((device, queue)) => {
                if let Err(msg) = crate::check_dimensions(
                    cfg.width,
                    cfg.height,
                    device.limits().max_texture_dimension_2d,
                ) {
                    crate::set_error(msg);
                    return std::ptr::null_mut();
                }
                crate::build_engine(cfg, device, queue, None, Some(VulkanState { format }))
            }
            Err(e) => {
                crate::set_error(e);
                std::ptr::null_mut()
            }
        }
    }));
    result.unwrap_or_else(|_| {
        crate::set_error("internal panic while creating the engine");
        std::ptr::null_mut()
    })
}

/// Render one frame into `image`, a `VkImage` of the engine's size and the `image_format` given to
/// `lactea_create_vulkan`, created with `VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT`. `image` is passed as
/// `uint64_t` (non-dispatchable handle). Contents and layout on entry do not matter; on exit the
/// image is in `VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL`. `wait` blocks until the GPU finished the
/// frame; without it the work is only submitted (same-queue ordering plus your own barrier is
/// enough, and the image must stay alive until that work has executed). `time` and `frame` as in
/// `lactea_render`.
///
/// # Safety
/// `engine` from `lactea_create_vulkan`; `image` a valid image of the device the engine runs on.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_render_to_vk_image(
    engine: *mut LacteaEngine,
    image: u64,
    time: f64,
    frame: u32,
    wait: bool,
) -> LacteaStatus {
    crate::guard_delivering(engine, || {
        let Some(e) = (unsafe { engine.as_mut() }) else {
            return fail(LacteaStatus::InvalidArgument, "engine is NULL");
        };
        if e.vulkan.is_none() {
            return fail(
                LacteaStatus::InvalidArgument,
                "engine was not created with lactea_create_vulkan: use lactea_render / lactea_render_to_buffer",
            );
        }
        if image == 0 {
            return fail(LacteaStatus::InvalidArgument, "image is VK_NULL_HANDLE");
        }
        imp::render_to_image(e, image, time, frame, wait)
    })
}

impl Drop for LacteaEngine {
    fn drop(&mut self) {
        // Host images and the host's device must outlive the GPU work lactea submitted. The wait is
        // bounded so a lost device cannot hang `lactea_destroy`.
        if self.vulkan.is_some() {
            let _ = self.device.poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(std::time::Duration::from_secs(5)),
            });
        }
    }
}

#[cfg(not(lactea_vulkan))]
mod imp {
    use super::*;

    const NO_VULKAN: &str = "Vulkan interop is not available on this platform";

    pub fn requirements(_: &LacteaVulkanHost) -> Result<Box<LacteaVulkanRequirements>, String> {
        Err(NO_VULKAN.into())
    }

    pub fn free_requirements(_: &LacteaVulkanRequirements) {}

    pub unsafe fn open_device(_: &LacteaVulkanHost) -> Result<(wgpu::Device, wgpu::Queue), String> {
        Err(NO_VULKAN.into())
    }

    pub fn render_to_image(_: &mut LacteaEngine, _: u64, _: f64, _: u32, _: bool) -> LacteaStatus {
        fail(LacteaStatus::Gpu, NO_VULKAN)
    }
}

#[cfg(lactea_vulkan)]
mod imp {
    use std::ffi::CStr;

    use ash::vk::{self, Handle};
    use wgpu::hal::api::Vulkan;
    use wgpu::hal::vulkan as hal;

    use super::*;

    /// Heap behind [`LacteaVulkanRequirements::private_data`].
    struct RequirementsData {
        /// Keeps the feature structs the `pNext` chain points into alive.
        _features: Box<hal::PhysicalDeviceFeatures>,
        _names: Vec<*const c_char>,
    }

    /// The host's extension name array, borrowed from its memory.
    unsafe fn host_names<'a>(
        ptr: *const *const c_char,
        count: u32,
    ) -> Result<Vec<&'a CStr>, String> {
        if count == 0 {
            return Ok(Vec::new());
        }
        if ptr.is_null() {
            return Err("extension name array is NULL".into());
        }
        Ok(unsafe { std::slice::from_raw_parts(ptr, count as usize) }
            .iter()
            .map(|&p| unsafe { CStr::from_ptr(p) })
            .collect())
    }

    /// The host's instance as a wgpu-hal instance (never destroyed by us) and its adapter.
    unsafe fn open_instance(
        host: &LacteaVulkanHost,
    ) -> Result<(hal::Instance, wgpu::hal::ExposedAdapter<Vulkan>), String> {
        if host.get_instance_proc_addr.is_null()
            || host.instance.is_null()
            || host.physical_device.is_null()
        {
            return Err(
                "host.get_instance_proc_addr, instance and physical_device must be set".into(),
            );
        }
        // SAFETY: the host promises `get_instance_proc_addr` is a valid
        // `PFN_vkGetInstanceProcAddr`; data and function pointers have the same size on every
        // platform with a Vulkan loader.
        let get_instance_proc_addr = unsafe {
            std::mem::transmute::<*const c_void, vk::PFN_vkGetInstanceProcAddr>(
                host.get_instance_proc_addr,
            )
        };
        let static_fn = || ash::StaticFn {
            get_instance_proc_addr,
        };
        let entry = unsafe { ash::Entry::from_static_fn(static_fn()) };
        let raw_instance = unsafe {
            ash::Instance::load(
                &static_fn(),
                vk::Instance::from_raw(host.instance as usize as u64),
            )
        };
        // wgpu-hal wants `&'static CStr` names and only ever looks for the ones it knows: hand it its
        // own list of what it would enable, restricted to what the host enabled.
        let host_extensions =
            unsafe { host_names(host.instance_extensions, host.instance_extension_count) }?;
        let api_version = if host.api_version == 0 {
            vk::API_VERSION_1_0
        } else {
            host.api_version
        };
        let flags = wgpu::InstanceFlags::empty();
        let mut extensions = hal::Instance::desired_extensions(&entry, api_version, flags)
            .map_err(|e| format!("enumerating instance extensions: {e}"))?;
        extensions.retain(|e| host_extensions.contains(e));
        let instance = unsafe {
            hal::Instance::from_raw(
                entry,
                raw_instance,
                api_version,
                0,
                None,
                extensions,
                flags,
                wgpu::MemoryBudgetThresholds::default(),
                false,
                // The host owns the VkInstance.
                Some(Box::new(|| {})),
            )
        }
        .map_err(|e| format!("wrapping the VkInstance: {e}"))?;
        let phd = vk::PhysicalDevice::from_raw(host.physical_device as usize as u64);
        // wgpu-hal assumes the device exposes every version the physical device reports; a lower
        // instance apiVersion hides the newer core functions (the loader returns NULL for them).
        let device_version = unsafe {
            instance
                .shared_instance()
                .raw_instance()
                .get_physical_device_properties(phd)
        }
        .api_version;
        let (have, need) = (
            (
                vk::api_version_major(api_version),
                vk::api_version_minor(api_version),
            ),
            (
                vk::api_version_major(device_version),
                vk::api_version_minor(device_version),
            ),
        );
        if have < need {
            return Err(format!(
                "the VkInstance was created with apiVersion {}.{} but the physical device is {}.{}: create it with \
                 apiVersion >= {}.{} (e.g. the value of vkEnumerateInstanceVersion)",
                have.0, have.1, need.0, need.1, need.0, need.1
            ));
        }
        let adapter = instance
            .expose_adapter(phd)
            .ok_or("wgpu cannot use this VkPhysicalDevice (below its minimum requirements)")?;
        Ok((instance, adapter))
    }

    pub fn requirements(host: &LacteaVulkanHost) -> Result<Box<LacteaVulkanRequirements>, String> {
        let (_instance, adapter) = unsafe { open_instance(host) }?;
        let features = wgpu::Features::empty();
        let exts = adapter.adapter.required_device_extensions(features);
        let mut phd_features = Box::new(adapter.adapter.physical_device_features(&exts, features));
        let (enabled_features, device_pnext) = {
            let info = phd_features.add_to_device_create(vk::DeviceCreateInfo::default());
            (info.p_enabled_features.cast::<c_void>(), info.p_next)
        };
        let names: Vec<*const c_char> = exts.iter().map(|e| e.as_ptr()).collect();
        let (names_ptr, count) = (names.as_ptr(), names.len() as u32);
        let data = Box::new(RequirementsData {
            _features: phd_features,
            _names: names,
        });
        Ok(Box::new(LacteaVulkanRequirements {
            device_extensions: names_ptr,
            device_extension_count: count,
            enabled_features,
            device_pnext,
            private_data: Box::into_raw(data).cast(),
        }))
    }

    pub fn free_requirements(req: &LacteaVulkanRequirements) {
        if !req.private_data.is_null() {
            drop(unsafe { Box::from_raw(req.private_data.cast::<RequirementsData>()) });
        }
    }

    pub unsafe fn open_device(
        host: &LacteaVulkanHost,
    ) -> Result<(wgpu::Device, wgpu::Queue), String> {
        if host.device.is_null() {
            return Err("host.device is NULL".into());
        }
        let (instance, exposed) = unsafe { open_instance(host) }?;
        let features = wgpu::Features::empty();
        // Exactly what wgpu itself enables on a device it opens for these features.
        let enabled = exposed.adapter.required_device_extensions(features);
        let host_extensions =
            unsafe { host_names(host.device_extensions, host.device_extension_count) }?;
        if let Some(missing) = enabled.iter().find(|e| !host_extensions.contains(e)) {
            return Err(format!(
                "the VkDevice must enable {} (see lactea_vulkan_requirements)",
                missing.to_string_lossy()
            ));
        }
        let raw_device = unsafe {
            ash::Device::load(
                instance.shared_instance().raw_instance().fp_v1_0(),
                vk::Device::from_raw(host.device as usize as u64),
            )
        };
        // wgpu's default limits: the baseline every device wgpu accepts must meet.
        let open = unsafe {
            exposed.adapter.device_from_raw(
                raw_device,
                // The host owns the VkDevice.
                Some(Box::new(|| {})),
                &enabled,
                features,
                &wgpu::Limits::default(),
                &wgpu::MemoryHints::default(),
                host.queue_family_index,
                host.queue_index,
            )
        }
        .map_err(|e| format!("wrapping the VkDevice: {e}"))?;
        let wgpu_instance = unsafe { wgpu::Instance::from_hal::<Vulkan>(instance) };
        let adapter = unsafe { wgpu_instance.create_adapter_from_hal::<Vulkan>(exposed) };
        unsafe {
            adapter.create_device_from_hal::<Vulkan>(open, &wgpu::DeviceDescriptor::default())
        }
        .map_err(|e| format!("create_device_from_hal: {e}"))
    }

    pub fn render_to_image(
        e: &mut LacteaEngine,
        image: u64,
        time: f64,
        frame: u32,
        wait: bool,
    ) -> LacteaStatus {
        let Some(format) = e.vulkan.as_ref().map(|v| v.format) else {
            return fail(LacteaStatus::InvalidArgument, "not a Vulkan engine");
        };
        let size = wgpu::Extent3d {
            width: e.width,
            height: e.height,
            depth_or_array_layers: 1,
        };
        let hal_desc = wgpu::hal::TextureDescriptor {
            label: Some("lactea host image"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUses::COLOR_TARGET,
            memory_flags: wgpu::hal::MemoryFlags::empty(),
            view_formats: Vec::new(),
        };
        let Some(hal_device) = (unsafe { e.device.as_hal::<Vulkan>() }) else {
            return fail(
                LacteaStatus::Gpu,
                "the engine's device is not a Vulkan device",
            );
        };
        // A fresh wrapper per call: wgpu then assumes an undefined layout, discards the old contents
        // and does not remember the layout the host moved the image to between frames.
        let hal_texture = unsafe {
            hal_device.texture_from_raw(
                vk::Image::from_raw(image),
                &hal_desc,
                // The host owns the VkImage.
                Some(Box::new(|| {})),
                hal::TextureMemory::External,
            )
        };
        drop(hal_device);
        let texture = unsafe {
            e.device.create_texture_from_hal::<Vulkan>(
                hal_texture,
                &wgpu::TextureDescriptor {
                    label: Some("lactea host image"),
                    size,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                    view_formats: &[],
                },
                wgpu::TextureUses::UNINITIALIZED,
            )
        };
        let view = texture.create_view(&Default::default());
        let (enc, request) = crate::encode_frame(e, &view, time, frame);
        e.queue.submit(Some(enc.finish()));
        drop(view);
        drop(texture);
        crate::notify_switch(e, request);
        if wait && let Err(err) = e.device.poll(wgpu::PollType::wait_indefinitely()) {
            return fail(LacteaStatus::Gpu, format!("poll: {err:?}"));
        }
        LacteaStatus::Ok
    }
}
