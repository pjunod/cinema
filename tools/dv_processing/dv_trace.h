#ifndef PLURX_DV_TRACE_H
#define PLURX_DV_TRACE_H

#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <libavutil/frame.h>
#include <libavutil/hwcontext.h>
#include <libplacebo/vulkan.h>

// GPU-decoded surfaces that the Vulkan device cannot import (another GPU,
// a software Vulkan, a driver without dma-buf import) are copied to host
// memory instead. Sticky for the window; the helper reports it in its
// decoder trace as "vaapi-host-copy".
static bool dv_hw_host_copy;
// The caller's planar ten-bit packer for a host copy of a decoded surface.
static uint16_t *(*dv_host_pack)(const AVFrame *);
static AVFrame *dv_host_frame(const AVFrame *surface) {
  AVFrame *host = av_frame_alloc();
  if (!host || av_hwframe_transfer_data(host, surface, 0) < 0 ||
      av_frame_copy_props(host, surface) < 0) {
    av_frame_free(&host);
    return NULL;
  }
  return host;
}

// Full pixel digests are quality diagnostics, not independent serving evidence.
static bool dv_frame_hashes(void) {
  const char *value = getenv("PLURX_DV_FRAME_HASHES");
  return value && strcmp(value, "1") == 0;
}

// Observe the actual selected device, including software Vulkan implementations.
static bool dv_gpu_runtime(pl_vulkan vk) {
  PFN_vkGetPhysicalDeviceProperties2 get_properties =
      (PFN_vkGetPhysicalDeviceProperties2)vk->get_proc_addr(
          vk->instance, "vkGetPhysicalDeviceProperties2");
  if (!get_properties)
    return false;
  VkPhysicalDeviceIDProperties ids = {
      .sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_ID_PROPERTIES};
  VkPhysicalDeviceProperties2 props = {
      .sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2, .pNext = &ids};
  get_properties(vk->phys_device, &props);
  char device[33], driver[33];
  for (unsigned i = 0; i < VK_UUID_SIZE; i++) {
    snprintf(device + 2 * i, 3, "%02x", ids.deviceUUID[i]);
    snprintf(driver + 2 * i, 3, "%02x", ids.driverUUID[i]);
  }
  printf("{\"kind\":\"gpu_runtime\",\"api_version\":%u,\"vendor_id\":%u,"
         "\"device_id\":%u,\"driver_version\":%u,\"device_type\":%u,"
         "\"device_uuid\":\"%s\",\"driver_uuid\":\"%s\"}\n",
         props.properties.apiVersion, props.properties.vendorID,
         props.properties.deviceID, props.properties.driverVersion,
         (unsigned)props.properties.deviceType, device, driver);
  return true;
}
#endif
