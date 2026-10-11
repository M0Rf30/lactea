/* Vulkan interop host: lactea renders into a VkImage this program created, on this program's
 * VkDevice. Steps (see crates/lactea-capi/src/vulkan.rs):
 *   1. VkInstance 1.1 + VK_KHR_get_physical_device_properties2
 *   2. lactea_vulkan_requirements() -> extensions + feature chain for vkCreateDevice
 *   3. lactea_create_vulkan()
 *   4. lactea_render_to_vk_image() per frame; read the image back and check it is not black.
 *
 * Build: cc vulkan.c -I../../include -L<dir with liblactea_capi> -llactea_capi -lvulkan -lm
 * Needs the Vulkan headers (vulkan-headers) and loader. Exit code 0 = frames rendered.
 * Set VK_VALIDATE=1 to enable VK_LAYER_KHRONOS_validation when it is installed. */

#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <vulkan/vulkan.h>

#include "lactea.h"

#define W 64
#define H 36

#define VK_CHECK(call)                                                                  \
    do {                                                                                \
        VkResult r_ = (call);                                                           \
        if (r_ != VK_SUCCESS) {                                                         \
            fprintf(stderr, "%s:%d: %s = %d\n", __FILE__, __LINE__, #call, (int)r_);    \
            return 1;                                                                   \
        }                                                                               \
    } while (0)

static uint32_t find_memory_type(VkPhysicalDevice pd, uint32_t bits, VkMemoryPropertyFlags want) {
    VkPhysicalDeviceMemoryProperties mp;
    vkGetPhysicalDeviceMemoryProperties(pd, &mp);
    for (uint32_t i = 0; i < mp.memoryTypeCount; i++)
        if ((bits & (1u << i)) && (mp.memoryTypes[i].propertyFlags & want) == want) return i;
    return UINT32_MAX;
}

static const char *PRESET =
    "MILKDROP_PRESET_VERSION=100\n"
    "fDecay=0.95\nfWaveAlpha=0.9\nnWaveMode=0\nzoom=1.02\nrot=0.01\nwarp=0.5\nfGammaAdj=1.5\n"
    "ob_a=0.5\nob_r=1\nob_size=0.02\nbDarkenCenter=1\n"
    "per_frame_1=zoom = 1.0 + 0.02*sin(time*3) + 0.01*bass;\n"
    "per_frame_2=q1 = q1 + 0.1; wave_r = 0.5+0.5*sin(time);\n";

static unsigned requested_calls;
static void on_requested(bool hard, void *user) {
    (void)hard;
    (*(unsigned *)user)++;
}

int main(int argc, char **argv) {
    /* "bgra" as the first argument renders into a B8G8R8A8 image instead of R8G8B8A8 */
    const int bgra = argc > 1 && strcmp(argv[1], "bgra") == 0;
    const VkFormat format = bgra ? VK_FORMAT_B8G8R8A8_UNORM : VK_FORMAT_R8G8B8A8_UNORM;
    /* 1. instance */
    const char *inst_exts[] = {VK_KHR_GET_PHYSICAL_DEVICE_PROPERTIES_2_EXTENSION_NAME};
    const char *layers[] = {"VK_LAYER_KHRONOS_validation"};
    /* The instance version must be >= the physical device's (wgpu assumes the device exposes every
     * version it reports): use whatever the loader supports. */
    uint32_t api = VK_API_VERSION_1_1;
    vkEnumerateInstanceVersion(&api);
    VkApplicationInfo app = {.sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
                             .pApplicationName = "lactea vulkan host",
                             .apiVersion = api};
    VkInstanceCreateInfo ici = {.sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
                                .pApplicationInfo = &app,
                                .enabledExtensionCount = 1,
                                .ppEnabledExtensionNames = inst_exts};
    if (getenv("VK_VALIDATE")) {
        ici.enabledLayerCount = 1;
        ici.ppEnabledLayerNames = layers;
    }
    VkInstance instance;
    VK_CHECK(vkCreateInstance(&ici, NULL, &instance));

    uint32_t n = 0;
    VK_CHECK(vkEnumeratePhysicalDevices(instance, &n, NULL));
    if (n == 0) {
        fprintf(stderr, "no Vulkan device\n");
        return 2;
    }
    VkPhysicalDevice *pds = calloc(n, sizeof *pds);
    VK_CHECK(vkEnumeratePhysicalDevices(instance, &n, pds));
    VkPhysicalDevice pd = pds[0];
    free(pds);

    uint32_t qn = 0;
    vkGetPhysicalDeviceQueueFamilyProperties(pd, &qn, NULL);
    VkQueueFamilyProperties *qf = calloc(qn, sizeof *qf);
    vkGetPhysicalDeviceQueueFamilyProperties(pd, &qn, qf);
    uint32_t family = UINT32_MAX;
    for (uint32_t i = 0; i < qn && family == UINT32_MAX; i++)
        if (qf[i].queueFlags & VK_QUEUE_GRAPHICS_BIT) family = i;
    free(qf);
    if (family == UINT32_MAX) {
        fprintf(stderr, "no graphics queue family\n");
        return 2;
    }

    /* 2. what lactea needs from the device */
    LacteaVulkanHost host;
    memset(&host, 0, sizeof host);
    host.get_instance_proc_addr = (const void *)vkGetInstanceProcAddr;
    host.instance = instance;
    host.physical_device = pd;
    host.queue_family_index = family;
    host.queue_index = 0;
    host.api_version = api;
    host.instance_extensions = inst_exts;
    host.instance_extension_count = 1;
    host.image_format = (uint32_t)format;

    LacteaVulkanRequirements *req = NULL;
    if (lactea_vulkan_requirements(&host, &req) != LACTEA_STATUS_OK) {
        fprintf(stderr, "lactea_vulkan_requirements: %s\n", lactea_last_error());
        return 1;
    }
    float prio = 1.0f;
    VkDeviceQueueCreateInfo qci = {.sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
                                   .queueFamilyIndex = family,
                                   .queueCount = 1,
                                   .pQueuePriorities = &prio};
    VkDeviceCreateInfo dci = {.sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,
                              .pNext = req->device_pnext,
                              .queueCreateInfoCount = 1,
                              .pQueueCreateInfos = &qci,
                              .enabledExtensionCount = req->device_extension_count,
                              .ppEnabledExtensionNames = req->device_extensions,
                              .pEnabledFeatures = req->enabled_features};
    VkDevice device;
    VK_CHECK(vkCreateDevice(pd, &dci, NULL, &device));
    VkQueue queue;
    vkGetDeviceQueue(device, family, 0, &queue);

    /* the image lactea renders into */
    VkImageCreateInfo imci = {.sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,
                              .imageType = VK_IMAGE_TYPE_2D,
                              .format = format,
                              .extent = {W, H, 1},
                              .mipLevels = 1,
                              .arrayLayers = 1,
                              .samples = VK_SAMPLE_COUNT_1_BIT,
                              .tiling = VK_IMAGE_TILING_OPTIMAL,
                              .usage = VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT,
                              .initialLayout = VK_IMAGE_LAYOUT_UNDEFINED};
    VkImage image;
    VK_CHECK(vkCreateImage(device, &imci, NULL, &image));
    VkMemoryRequirements mr;
    vkGetImageMemoryRequirements(device, image, &mr);
    VkMemoryAllocateInfo mai = {.sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
                                .allocationSize = mr.size,
                                .memoryTypeIndex = find_memory_type(pd, mr.memoryTypeBits, VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT)};
    VkDeviceMemory image_mem;
    VK_CHECK(vkAllocateMemory(device, &mai, NULL, &image_mem));
    VK_CHECK(vkBindImageMemory(device, image, image_mem, 0));

    /* read-back buffer */
    VkBufferCreateInfo bci = {.sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,
                              .size = W * H * 4,
                              .usage = VK_BUFFER_USAGE_TRANSFER_DST_BIT};
    VkBuffer buf;
    VK_CHECK(vkCreateBuffer(device, &bci, NULL, &buf));
    vkGetBufferMemoryRequirements(device, buf, &mr);
    mai.allocationSize = mr.size;
    mai.memoryTypeIndex = find_memory_type(pd, mr.memoryTypeBits,
                                           VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT);
    VkDeviceMemory buf_mem;
    VK_CHECK(vkAllocateMemory(device, &mai, NULL, &buf_mem));
    VK_CHECK(vkBindBufferMemory(device, buf, buf_mem, 0));

    VkCommandPoolCreateInfo cpci = {.sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO, .queueFamilyIndex = family};
    VkCommandPool pool;
    VK_CHECK(vkCreateCommandPool(device, &cpci, NULL, &pool));

    /* 3. the engine, on our device */
    LacteaConfig cfg;
    lactea_config_default(&cfg);
    cfg.width = W;
    cfg.height = H;
    cfg.mesh_x = 16;
    cfg.mesh_y = 12;
    host.device = device;
    host.device_extensions = req->device_extensions;
    host.device_extension_count = req->device_extension_count;
    LacteaEngine *engine = lactea_create_vulkan(&cfg, &host);
    if (!engine) {
        fprintf(stderr, "lactea_create_vulkan: %s\n", lactea_last_error());
        return 1;
    }
    lactea_set_switch_requested_callback(engine, on_requested, &requested_calls);
    if (lactea_load_preset(engine, (const uint8_t *)PRESET, strlen(PRESET), "vk") != LACTEA_STATUS_OK) {
        fprintf(stderr, "lactea_load_preset: %s\n", lactea_last_error());
        return 1;
    }
    /* an engine without an internal target refuses the CPU path */
    if (lactea_render(engine, 0.0, 0) != LACTEA_STATUS_INVALID_ARGUMENT) {
        fprintf(stderr, "lactea_render must be refused on a Vulkan engine\n");
        return 1;
    }

    /* 4. frames */
    float pcm[2 * 512];
    for (int f = 0; f < 10; f++) {
        for (int i = 0; i < 512; i++) pcm[2 * i] = pcm[2 * i + 1] = 0.5f * sinf((float)(f * 512 + i) * 0.05f);
        lactea_push_pcm_f32(engine, pcm, 512, 2);
        if (lactea_render_to_vk_image(engine, (uint64_t)image, f / 60.0, (uint32_t)f, true) != LACTEA_STATUS_OK) {
            fprintf(stderr, "lactea_render_to_vk_image: %s\n", lactea_last_error());
            return 1;
        }
    }

    /* the image is in COLOR_ATTACHMENT_OPTIMAL: copy it out */
    VkCommandBufferAllocateInfo cbai = {.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
                                        .commandPool = pool,
                                        .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY,
                                        .commandBufferCount = 1};
    VkCommandBuffer cb;
    VK_CHECK(vkAllocateCommandBuffers(device, &cbai, &cb));
    VkCommandBufferBeginInfo cbbi = {.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO,
                                     .flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT};
    VK_CHECK(vkBeginCommandBuffer(cb, &cbbi));
    VkImageMemoryBarrier b = {.sType = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER,
                              .srcAccessMask = VK_ACCESS_COLOR_ATTACHMENT_WRITE_BIT,
                              .dstAccessMask = VK_ACCESS_TRANSFER_READ_BIT,
                              .oldLayout = VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
                              .newLayout = VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
                              .srcQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
                              .dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
                              .image = image,
                              .subresourceRange = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1}};
    vkCmdPipelineBarrier(cb, VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 0, NULL, 0,
                         NULL, 1, &b);
    VkBufferImageCopy copy = {.imageSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1}, .imageExtent = {W, H, 1}};
    vkCmdCopyImageToBuffer(cb, image, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, buf, 1, &copy);
    VK_CHECK(vkEndCommandBuffer(cb));
    VkSubmitInfo si = {.sType = VK_STRUCTURE_TYPE_SUBMIT_INFO, .commandBufferCount = 1, .pCommandBuffers = &cb};
    VK_CHECK(vkQueueSubmit(queue, 1, &si, VK_NULL_HANDLE));
    VK_CHECK(vkQueueWaitIdle(queue));

    unsigned char *px;
    VK_CHECK(vkMapMemory(device, buf_mem, 0, VK_WHOLE_SIZE, 0, (void **)&px));
    unsigned lit = 0;
    for (int i = 0; i < W * H; i++)
        if (px[4 * i] | px[4 * i + 1] | px[4 * i + 2]) lit++;
    vkUnmapMemory(device, buf_mem);
    printf("vulkan host: %u/%d pixels lit after 10 frames\n", lit, W * H);

    /* teardown order: engine, then the device it runs on */
    lactea_destroy(engine);
    lactea_vulkan_requirements_free(req);
    vkDestroyCommandPool(device, pool, NULL);
    vkDestroyBuffer(device, buf, NULL);
    vkFreeMemory(device, buf_mem, NULL);
    vkDestroyImage(device, image, NULL);
    vkFreeMemory(device, image_mem, NULL);
    vkDestroyDevice(device, NULL);
    vkDestroyInstance(instance, NULL);
    return lit == 0 ? 1 : 0;
}
