/*
 * wined3d without OpenGL or Vulkan: the browser build of wined3d
 * (WINED3D_WEBGPU_ONLY) leaves out the OpenGL and Vulkan backends and draws
 * with adapter_wgpu.c, or without 3D (no3d) when there is no WebGPU.
 *
 * wined3d's shared code (buffers, textures, views, queries, the device)
 * still names some of those backends' functions in paths only their
 * adapters reach. Those adapters are never created, so these stubs only
 * satisfy the linker. If one is ever reached, it says so and stops the process.
 *
 * wined3d also exports vkd3d's Direct3D 12 API (for d3d12.dll and dxgi's
 * Direct3D 12 swapchains), which runs on Vulkan. Defining those exports
 * here keeps vkd3d's Direct3D 12 implementation out of the link; they fail
 * as they would without a Vulkan driver. vkd3d-shader stays: it compiles
 * wined3d's fixed-function HLSL and serves d3dcompiler. Of its targets and
 * sources, SPIR-V (Vulkan), GLSL, MSL and DXIL (shader model 6, Direct3D
 * 12 only) are left out the same way.
 *
 * Copyright 2026 the WebWindows authors
 *
 * This library is free software; you can redistribute it and/or
 * modify it under the terms of the GNU Lesser General Public
 * License as published by the Free Software Foundation; either
 * version 2.1 of the License, or (at your option) any later version.
 */

#include <stdarg.h>
#include <stdlib.h>
#include "windef.h"
#include "winbase.h"
#include "wine/debug.h"

WINE_DEFAULT_DEBUG_CHANNEL(d3d);

/* The thread-local index for the current OpenGL context: wined3d_main.c
 * allocates it at load and frees it at unload. */
static DWORD wined3d_context_tls_idx;

DWORD context_get_tls_idx(void)
{
    return wined3d_context_tls_idx;
}

void context_set_tls_idx(DWORD idx)
{
    wined3d_context_tls_idx = idx;
}

/* Called at thread detach only when opengl32 is loaded; nothing of OpenGL
 * is current. */
BOOL wined3d_context_gl_set_current(void *context)
{
    return TRUE;
}

#define NOGL_STUB(name) \
    void name(void); \
    void name(void) \
    { \
        ERR(#name " called, but this wined3d has no OpenGL or Vulkan.\n"); \
        abort(); \
    }

/* OpenGL */
NOGL_STUB(context_state_drawbuf)
NOGL_STUB(context_state_fb)
NOGL_STUB(print_glsl_info_log)
NOGL_STUB(shader_glsl_validate_link)
NOGL_STUB(wined3d_check_gl_call)
NOGL_STUB(wined3d_context_gl_active_texture)
NOGL_STUB(wined3d_context_gl_alloc_fence)
NOGL_STUB(wined3d_context_gl_alloc_occlusion_query)
NOGL_STUB(wined3d_context_gl_alloc_pipeline_statistics_query)
NOGL_STUB(wined3d_context_gl_alloc_so_statistics_query)
NOGL_STUB(wined3d_context_gl_alloc_timestamp_query)
NOGL_STUB(wined3d_context_gl_allocate_vram_chunk_buffer)
NOGL_STUB(wined3d_context_gl_apply_fbo_state_explicit)
NOGL_STUB(wined3d_context_gl_bind_bo)
NOGL_STUB(wined3d_context_gl_bind_dummy_textures)
NOGL_STUB(wined3d_context_gl_bind_texture)
NOGL_STUB(wined3d_context_gl_check_fbo_status)
NOGL_STUB(wined3d_context_gl_destroy)
NOGL_STUB(wined3d_context_gl_destroy_bo)
NOGL_STUB(wined3d_context_gl_end_transform_feedback)
NOGL_STUB(wined3d_context_gl_free_fence)
NOGL_STUB(wined3d_context_gl_free_occlusion_query)
NOGL_STUB(wined3d_context_gl_free_pipeline_statistics_query)
NOGL_STUB(wined3d_context_gl_free_so_statistics_query)
NOGL_STUB(wined3d_context_gl_free_timestamp_query)
NOGL_STUB(wined3d_context_gl_get_offscreen_gl_buffer)
NOGL_STUB(wined3d_context_gl_init)
NOGL_STUB(wined3d_context_gl_reacquire)
NOGL_STUB(wined3d_context_gl_submit_command_fence)
NOGL_STUB(wined3d_context_gl_update_stream_sources)
NOGL_STUB(wined3d_context_gl_wait_command_fence)
NOGL_STUB(wined3d_gl_texture_swizzle_from_color_fixup)
NOGL_STUB(wined3d_glsl_blitter_create)
NOGL_STUB(wined3d_texture_get_gl_buffer)
NOGL_STUB(wined3d_texture_gl_apply_sampler_desc)
NOGL_STUB(wined3d_texture_gl_bind)
NOGL_STUB(wined3d_texture_gl_bind_and_dirtify)
NOGL_STUB(wined3d_texture_gl_prepare_texture)

/* Vulkan */
NOGL_STUB(adapter_vk_copy_bo_address)
NOGL_STUB(vk_compare_op_from_wined3d)
NOGL_STUB(wined3d_adapter_vk_get_memory_type_index)
NOGL_STUB(wined3d_aux_command_pool_vk_get_buffer)
NOGL_STUB(wined3d_aux_command_pool_vk_retire_buffer)
NOGL_STUB(wined3d_context_vk_allocate_memory)
NOGL_STUB(wined3d_context_vk_allocate_query)
NOGL_STUB(wined3d_context_vk_create_bo)
NOGL_STUB(wined3d_context_vk_create_image)
NOGL_STUB(wined3d_context_vk_create_vk_descriptor_set)
NOGL_STUB(wined3d_context_vk_destroy_bo)
NOGL_STUB(wined3d_context_vk_destroy_image)
NOGL_STUB(wined3d_context_vk_destroy_vk_buffer_view)
NOGL_STUB(wined3d_context_vk_destroy_vk_event)
NOGL_STUB(wined3d_context_vk_destroy_vk_image_view)
NOGL_STUB(wined3d_context_vk_destroy_vk_video_parameters)
NOGL_STUB(wined3d_context_vk_destroy_vk_video_session)
NOGL_STUB(wined3d_context_vk_end_current_render_pass)
NOGL_STUB(wined3d_context_vk_free_memory)
NOGL_STUB(wined3d_context_vk_get_command_buffer)
NOGL_STUB(wined3d_context_vk_get_pipeline_layout)
NOGL_STUB(wined3d_context_vk_image_barrier)
NOGL_STUB(wined3d_context_vk_submit_command_buffer)
NOGL_STUB(wined3d_context_vk_wait_command_buffer)
NOGL_STUB(wined3d_fbo_blitter_create)
NOGL_STUB(wined3d_ffp_blitter_create)
NOGL_STUB(wined3d_raw_blitter_create)
NOGL_STUB(wined3d_texture_vk_get_default_image_info)
NOGL_STUB(wined3d_texture_vk_prepare_texture)
NOGL_STUB(wined3d_vk_swizzle_from_color_fixup)

/* vkd3d's Direct3D 12 exports (wined3d.spec), and vkd3d_set_log_callback(),
 * which vkd3d-utils calls and which lives next to them in vkd3d_main.c. */
void vkd3d_shader_set_log_callback(void *callback);
void vkd3d_dbg_set_log_callback(void *callback);

void vkd3d_set_log_callback(void *callback)
{
    vkd3d_shader_set_log_callback(callback);
    vkd3d_dbg_set_log_callback(callback);
}

#define VKD3D_E_NOTIMPL ((LONG)0x80004001)

LONG vkd3d_create_instance(const void *info, void **instance)
{
    WARN("No Vulkan, no Direct3D 12.\n");
    return VKD3D_E_NOTIMPL;
}

LONG vkd3d_create_device(const void *info, const void *iid, void **device)
{
    WARN("No Vulkan, no Direct3D 12.\n");
    return VKD3D_E_NOTIMPL;
}

LONG vkd3d_create_image_resource(void *device, const void *info, void **resource) { return VKD3D_E_NOTIMPL; }
LONG vkd3d_create_root_signature_deserializer(const void *data, SIZE_T size, const void *iid, void **d) { return VKD3D_E_NOTIMPL; }
LONG vkd3d_create_versioned_root_signature_deserializer(const void *data, SIZE_T size, const void *iid, void **d) { return VKD3D_E_NOTIMPL; }
LONG vkd3d_serialize_root_signature(const void *desc, unsigned int version, void **blob, void **error) { return VKD3D_E_NOTIMPL; }
LONG vkd3d_serialize_versioned_root_signature(const void *desc, void **blob, void **error) { return VKD3D_E_NOTIMPL; }
LONG vkd3d_queue_signal_on_cpu(void *queue, void *fence, UINT64 value) { return VKD3D_E_NOTIMPL; }
void *vkd3d_acquire_vk_queue(void *queue) { return NULL; }
void vkd3d_release_vk_queue(void *queue) {}
void *vkd3d_get_device_parent(void *device) { return NULL; }
void *vkd3d_get_vk_device(void *device) { return NULL; }
void *vkd3d_get_vk_physical_device(void *device) { return NULL; }
UINT32 vkd3d_get_vk_queue_family_index(void *queue) { return 0; }
unsigned int vkd3d_get_dxgi_format(unsigned int format) { return 0; }
unsigned int vkd3d_get_vk_format(unsigned int format) { return 0; }
ULONG vkd3d_instance_decref(void *instance) { return 0; }
ULONG vkd3d_instance_incref(void *instance) { return 0; }
void *vkd3d_instance_from_device(void *device) { return NULL; }
UINT64 vkd3d_instance_get_vk_instance(void *instance) { return 0; }
ULONG vkd3d_resource_decref(void *resource) { return 0; }
ULONG vkd3d_resource_incref(void *resource) { return 0; }

/* vkd3d-shader's SPIR-V, GLSL and MSL backends and its DXIL parser, each
 * called only from vkd3d_shader_main.c. */
#define VKD3D_ERROR_NOT_IMPLEMENTED (-5)

int spirv_compile(void *program, UINT64 config_flags, const void *compile_info, void *out, void *message_context)
{
    return VKD3D_ERROR_NOT_IMPLEMENTED;
}

int glsl_compile(void *program, UINT64 config_flags, const void *combined_sampler_info,
        const void *compile_info, void *out, void *message_context)
{
    return VKD3D_ERROR_NOT_IMPLEMENTED;
}

int msl_compile(void *program, UINT64 config_flags, const void *compile_info, void *out, void *message_context)
{
    return VKD3D_ERROR_NOT_IMPLEMENTED;
}

int dxil_parse(const void *compile_info, UINT64 config_flags, void *message_context, void *program)
{
    return VKD3D_ERROR_NOT_IMPLEMENTED;
}
