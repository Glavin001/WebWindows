/*
 * wined3d backend for WebGPU, through the d3dgpu command stream.
 *
 * The backend runs where wined3d runs (translated, on the guest side). It
 * turns wined3d's resources, state and draws into d3dgpu protocol commands
 * (crates/d3dgpu-proto, include/d3dgpu_proto.h) and hands each batch to the
 * host with one unix call; the host's render worker executes it on WebGPU
 * with the d3dgpu render core.
 *
 * Model:
 *  - Buffers keep a CPU shadow in their buffer object; every write to it
 *    (uploads, unmaps) is mirrored with a WriteBuffer command.
 *  - Textures have a GPU copy (WINED3D_LOCATION_TEXTURE_RGB) next to
 *    wined3d's system memory copy. Loading TEXTURE_RGB uploads with
 *    WriteTexture; loading SYSMEM from TEXTURE_RGB reads back with
 *    ReadTexture and waits for its fence.
 *  - Shaders are Direct3D 9 bytecode, the application's or wined3d's
 *    fixed-function HLSL compiled by vkd3d-shader (ffp_hlsl); the core
 *    translates them. Constants come from wined3d's push constant buffers.
 *  - Present reads the back buffer back and draws it into the window with
 *    GDI, so it composes with the rest of the desktop.
 *
 * Copyright 2026 the WebWindows authors
 *
 * This library is free software; you can redistribute it and/or
 * modify it under the terms of the GNU Lesser General Public
 * License as published by the Free Software Foundation; either
 * version 2.1 of the License, or (at your option) any later version.
 */

#include "wined3d_private.h"
#include "wine/unixlib.h"
#include "d3dgpu_proto.h"

WINE_DEFAULT_DEBUG_CHANNEL(d3d);

/* ---- Transport --------------------------------------------------------- */

/* Unix calls the host implements for wined3d.dll (runtime/wine/d3d.mjs). */
enum wgpu_unix_call
{
    unix_wgpu_open,
    unix_wgpu_submit,
    unix_wgpu_wait,
};

struct wgpu_open_params
{
    UINT32 version;        /* in: D3DGPU_VERSION */
    UINT32 max_batch;      /* out: largest batch the host takes */
    UINT32 shared_size;    /* out: bytes of the readback region */
    char name[64];         /* out: adapter description */
};

struct wgpu_submit_params
{
    UINT32 data;
    UINT32 size;
};

struct wgpu_wait_params
{
    UINT32 fence_lo, fence_hi;
    UINT32 shared_offset;  /* copy this much of the readback region ... */
    UINT32 size;
    UINT32 dst;            /* ... here once the fence completed (if size) */
};

static struct wgpu_open_params wgpu_host;

/* Unique d3dgpu object ids for the process (handles are global to a core). */
static LONG wgpu_next_id;

static uint32_t wgpu_alloc_id(void)
{
    return (uint32_t)InterlockedIncrement(&wgpu_next_id);
}

struct wgpu_vertex_decl
{
    const struct wined3d_vertex_declaration *decl;
    uint32_t hash, id;
};

struct wined3d_device_wgpu
{
    struct wined3d_device d;
    struct wined3d_context context;

    BYTE *cmds;
    size_t cmd_size, cmd_capacity;
    uint64_t fence;

    /* What the core has, to skip redundant commands. */
    uint32_t vs_id, ps_id, decl_id;
    float consts_f[2][WINED3D_MAX_VS_CONSTS_F][4];
    unsigned int consts_f_count[2];
    int consts_i[2][WINED3D_MAX_CONSTS_I][4];
    uint32_t consts_b[2][WINED3D_MAX_CONSTS_B];
    bool consts_valid;

    struct wgpu_vertex_decl *decls;
    SIZE_T decl_count, decls_size;
};

static inline struct wined3d_device_wgpu *wined3d_device_wgpu(struct wined3d_device *device)
{
    return CONTAINING_RECORD(device, struct wined3d_device_wgpu, d);
}

static void wgpu_flush(struct wined3d_device_wgpu *device)
{
    struct d3dgpu_batch_header *h = (struct d3dgpu_batch_header *)device->cmds;
    struct wgpu_submit_params params;

    if (device->cmd_size <= sizeof(*h))
        return;
    h->magic = D3DGPU_MAGIC;
    h->version = D3DGPU_VERSION;
    h->len = device->cmd_size;
    params.data = (UINT32)(ULONG_PTR)device->cmds;
    params.size = device->cmd_size;
    if (WINE_UNIX_CALL(unix_wgpu_submit, &params))
        ERR("Failed to submit a batch of %u bytes.\n", params.size);
    device->cmd_size = sizeof(*h);
}

/* Room for a command of `size` bytes (rounded up to 4); fills its header. */
static void *wgpu_cmd(struct wined3d_device_wgpu *device, uint32_t op, size_t size)
{
    struct d3dgpu_cmd_header *h;

    size = (size + 3) & ~(size_t)3;
    if (device->cmd_size + size > wgpu_host.max_batch)
        wgpu_flush(device);
    if (device->cmd_size + size > device->cmd_capacity)
    {
        size_t capacity = max(device->cmd_capacity * 2, device->cmd_size + size);
        BYTE *cmds;

        if (!(cmds = realloc(device->cmds, capacity)))
        {
            ERR("Out of memory for a %Iu byte command.\n", size);
            return NULL;
        }
        device->cmds = cmds;
        device->cmd_capacity = capacity;
    }
    h = (struct d3dgpu_cmd_header *)(device->cmds + device->cmd_size);
    memset(h, 0, size);
    h->op = op;
    h->size = size;
    device->cmd_size += size;
    return h;
}

/* A command whose fixed part is `fixed` bytes, followed by inline data. */
static void *wgpu_cmd_data(struct wined3d_device_wgpu *device, uint32_t op, size_t fixed,
        const void *data, uint32_t len)
{
    BYTE *cmd;

    if (!(cmd = wgpu_cmd(device, op, fixed + 8 + len)))
        return NULL;
    *(uint32_t *)(cmd + fixed) = D3DGPU_DATA_INLINE;
    *(uint32_t *)(cmd + fixed + 4) = len;
    if (data)
        memcpy(cmd + fixed + 8, data, len);
    return cmd;
}

/* Direct3D 9 clears only inside the viewport: cover the whole target. */
static void wgpu_full_viewport(struct wined3d_device_wgpu *device, unsigned int width, unsigned int height)
{
    struct d3dgpu_cmd_set_viewport *cmd;

    if ((cmd = wgpu_cmd(device, D3DGPU_OP_SET_VIEWPORT, sizeof(*cmd))))
    {
        cmd->width = width;
        cmd->height = height;
        cmd->min_z = 0.0f;
        cmd->max_z = 1.0f;
    }
}

static void wgpu_destroy_object(struct wined3d_device_wgpu *device, uint32_t id)
{
    struct d3dgpu_cmd_destroy *cmd;

    if (id && (cmd = wgpu_cmd(device, D3DGPU_OP_DESTROY, sizeof(*cmd))))
        cmd->id = id;
}

static void wgpu_set_render_state(struct wined3d_device_wgpu *device, uint32_t state, uint32_t value)
{
    struct d3dgpu_cmd_set_render_state *cmd;

    if ((cmd = wgpu_cmd(device, D3DGPU_OP_SET_RENDER_STATE, sizeof(*cmd))))
    {
        cmd->state = state;
        cmd->value = value;
    }
}

/* Signals a fence and waits for it, then copies `size` bytes of the
 * readback region at `offset` to `dst`. */
static void wgpu_wait(struct wined3d_device_wgpu *device, uint32_t offset, uint32_t size, void *dst)
{
    struct d3dgpu_cmd_signal *cmd;
    struct wgpu_wait_params params;

    ++device->fence;
    if ((cmd = wgpu_cmd(device, D3DGPU_OP_SIGNAL, sizeof(*cmd))))
    {
        cmd->fence_lo = device->fence;
        cmd->fence_hi = device->fence >> 32;
    }
    wgpu_flush(device);
    params.fence_lo = device->fence;
    params.fence_hi = device->fence >> 32;
    params.shared_offset = offset;
    params.size = size;
    params.dst = (UINT32)(ULONG_PTR)dst;
    if (WINE_UNIX_CALL(unix_wgpu_wait, &params))
        ERR("Failed to wait for fence %s.\n", wine_dbgstr_longlong(device->fence));
}

/* ---- Formats ----------------------------------------------------------- */

static uint32_t d3dformat_from_wined3d(enum wined3d_format_id format)
{
    const BYTE *c = (const BYTE *)&format;

    /* FOURCC formats (DXTn, INTZ, ...) are the same in both. */
    if (isprint(c[0]) && isprint(c[1]) && isprint(c[2]) && isprint(c[3]))
        return format;

    switch (format)
    {
        case WINED3DFMT_B8G8R8_UNORM: return 20;            /* D3DFMT_R8G8B8 */
        case WINED3DFMT_B8G8R8A8_UNORM: return 21;          /* D3DFMT_A8R8G8B8 */
        case WINED3DFMT_B8G8R8X8_UNORM: return 22;          /* D3DFMT_X8R8G8B8 */
        case WINED3DFMT_B5G6R5_UNORM: return 23;            /* D3DFMT_R5G6B5 */
        case WINED3DFMT_B5G5R5X1_UNORM: return 24;          /* D3DFMT_X1R5G5B5 */
        case WINED3DFMT_B5G5R5A1_UNORM: return 25;          /* D3DFMT_A1R5G5B5 */
        case WINED3DFMT_B4G4R4A4_UNORM: return 26;          /* D3DFMT_A4R4G4B4 */
        case WINED3DFMT_B2G3R3_UNORM: return 27;            /* D3DFMT_R3G3B2 */
        case WINED3DFMT_A8_UNORM: return 28;                /* D3DFMT_A8 */
        case WINED3DFMT_B2G3R3A8_UNORM: return 29;          /* D3DFMT_A8R3G3B2 */
        case WINED3DFMT_B4G4R4X4_UNORM: return 30;          /* D3DFMT_X4R4G4B4 */
        case WINED3DFMT_R10G10B10A2_UNORM: return 31;       /* D3DFMT_A2B10G10R10 */
        case WINED3DFMT_R8G8B8A8_UNORM: return 32;          /* D3DFMT_A8B8G8R8 */
        case WINED3DFMT_R8G8B8X8_UNORM: return 33;          /* D3DFMT_X8B8G8R8 */
        case WINED3DFMT_R16G16_UNORM: return 34;            /* D3DFMT_G16R16 */
        case WINED3DFMT_B10G10R10A2_UNORM: return 35;       /* D3DFMT_A2R10G10B10 */
        case WINED3DFMT_R16G16B16A16_UNORM: return 36;      /* D3DFMT_A16B16G16R16 */
        case WINED3DFMT_P8_UINT_A8_UNORM: return 40;        /* D3DFMT_A8P8 */
        case WINED3DFMT_P8_UINT: return 41;                 /* D3DFMT_P8 */
        case WINED3DFMT_L8_UNORM: return 50;                /* D3DFMT_L8 */
        case WINED3DFMT_L8A8_UNORM: return 51;              /* D3DFMT_A8L8 */
        case WINED3DFMT_L4A4_UNORM: return 52;              /* D3DFMT_A4L4 */
        case WINED3DFMT_R8G8_SNORM: return 60;              /* D3DFMT_V8U8 */
        case WINED3DFMT_R5G5_SNORM_L6_UNORM: return 61;     /* D3DFMT_L6V5U5 */
        case WINED3DFMT_R8G8_SNORM_L8X8_UNORM: return 62;   /* D3DFMT_X8L8V8U8 */
        case WINED3DFMT_R8G8B8A8_SNORM: return 63;          /* D3DFMT_Q8W8V8U8 */
        case WINED3DFMT_R16G16_SNORM: return 64;            /* D3DFMT_V16U16 */
        case WINED3DFMT_R10G10B10_SNORM_A2_UNORM: return 67; /* D3DFMT_A2W10V10U10 */
        case WINED3DFMT_D16_LOCKABLE: return 70;            /* D3DFMT_D16_LOCKABLE */
        case WINED3DFMT_D32_UNORM: return 71;               /* D3DFMT_D32 */
        case WINED3DFMT_S1_UINT_D15_UNORM: return 73;       /* D3DFMT_D15S1 */
        case WINED3DFMT_D24_UNORM_S8_UINT: return 75;       /* D3DFMT_D24S8 */
        case WINED3DFMT_X8D24_UNORM: return 77;             /* D3DFMT_D24X8 */
        case WINED3DFMT_S4X4_UINT_D24_UNORM: return 79;     /* D3DFMT_D24X4S4 */
        case WINED3DFMT_D16_UNORM: return 80;               /* D3DFMT_D16 */
        case WINED3DFMT_D32_FLOAT: return 82;               /* D3DFMT_D32F_LOCKABLE */
        case WINED3DFMT_S8_UINT_D24_FLOAT: return 83;       /* D3DFMT_D24FS8 */
        case WINED3DFMT_L16_UNORM: return 81;               /* D3DFMT_L16 */
        case WINED3DFMT_R16G16B16A16_SNORM: return 110;     /* D3DFMT_Q16W16V16U16 */
        case WINED3DFMT_R16_FLOAT: return 111;              /* D3DFMT_R16F */
        case WINED3DFMT_R16G16_FLOAT: return 112;           /* D3DFMT_G16R16F */
        case WINED3DFMT_R16G16B16A16_FLOAT: return 113;     /* D3DFMT_A16B16G16R16F */
        case WINED3DFMT_R32_FLOAT: return 114;              /* D3DFMT_R32F */
        case WINED3DFMT_R32G32_FLOAT: return 115;           /* D3DFMT_G32R32F */
        case WINED3DFMT_R32G32B32A32_FLOAT: return 116;     /* D3DFMT_A32B32G32R32F */
        default:
            return 0;
    }
}

/* D3DDECLTYPE of a vertex element format, or ~0u. */
static uint32_t decl_type_from_wined3d(enum wined3d_format_id format)
{
    switch (format)
    {
        case WINED3DFMT_R32_FLOAT: return 0;
        case WINED3DFMT_R32G32_FLOAT: return 1;
        case WINED3DFMT_R32G32B32_FLOAT: return 2;
        case WINED3DFMT_R32G32B32A32_FLOAT: return 3;
        case WINED3DFMT_B8G8R8A8_UNORM: return 4;
        case WINED3DFMT_R8G8B8A8_UINT: return 5;
        case WINED3DFMT_R16G16_SINT: return 6;
        case WINED3DFMT_R16G16B16A16_SINT: return 7;
        case WINED3DFMT_R8G8B8A8_UNORM: return 8;
        case WINED3DFMT_R16G16_SNORM: return 9;
        case WINED3DFMT_R16G16B16A16_SNORM: return 10;
        case WINED3DFMT_R16G16_UNORM: return 11;
        case WINED3DFMT_R16G16B16A16_UNORM: return 12;
        case WINED3DFMT_R10G10B10X2_UINT: return 13;
        case WINED3DFMT_R10G10B10X2_SNORM: return 14;
        case WINED3DFMT_R16G16_FLOAT: return 15;
        case WINED3DFMT_R16G16B16A16_FLOAT: return 16;
        default: return ~0u;
    }
}

static uint32_t d3dcolor_from_wined3d(const struct wined3d_color *c)
{
    return (uint32_t)(lrintf(min(max(c->a, 0.0f), 1.0f) * 255.0f)) << 24
            | (uint32_t)(lrintf(min(max(c->r, 0.0f), 1.0f) * 255.0f)) << 16
            | (uint32_t)(lrintf(min(max(c->g, 0.0f), 1.0f) * 255.0f)) << 8
            | (uint32_t)(lrintf(min(max(c->b, 0.0f), 1.0f) * 255.0f));
}

static uint32_t float_bits(float f)
{
    union { float f; uint32_t u; } u = {.f = f};
    return u.u;
}

/* ---- Buffers ----------------------------------------------------------- */

struct wined3d_bo_wgpu
{
    struct wined3d_bo b;
    uint32_t id;
    size_t size;
    BYTE *shadow;
};

static inline struct wined3d_bo_wgpu *wined3d_bo_wgpu(struct wined3d_bo *bo)
{
    return CONTAINING_RECORD(bo, struct wined3d_bo_wgpu, b);
}

static void wgpu_write_bo(struct wined3d_device_wgpu *device, struct wined3d_bo_wgpu *bo,
        size_t offset, size_t size)
{
    if (!size || offset >= bo->size)
        return;
    size = min(size, bo->size - offset);
    /* WriteBuffer offsets and sizes are multiples of 4. */
    size = ((offset + size + 3) & ~(size_t)3) - (offset & ~(size_t)3);
    offset &= ~(size_t)3;
    size = min(size, ((bo->size + 3) & ~(size_t)3) - offset);
    while (size)
    {
        size_t chunk = min(size, wgpu_host.max_batch / 2);
        struct d3dgpu_cmd_write_buffer *cmd;

        if (!(cmd = wgpu_cmd_data(device, D3DGPU_OP_WRITE_BUFFER, sizeof(*cmd), bo->shadow + offset, chunk)))
            return;
        cmd->id = bo->id;
        cmd->offset = offset;
        offset += chunk;
        size -= chunk;
    }
}

BOOL wined3d_buffer_wgpu_prepare_location(struct wined3d_buffer *buffer,
        struct wined3d_context *context, unsigned int location)
{
    struct wined3d_device_wgpu *device = wined3d_device_wgpu(buffer->resource.device);
    struct d3dgpu_cmd_create_buffer *cmd;
    struct wined3d_bo_wgpu *bo;
    uint32_t usage = 0;

    switch (location)
    {
        case WINED3D_LOCATION_SYSMEM:
            return wined3d_resource_prepare_sysmem(&buffer->resource);

        case WINED3D_LOCATION_BUFFER:
            if (buffer->buffer_object)
                return TRUE;
            if (!(bo = calloc(1, sizeof(*bo))))
                return FALSE;
            bo->size = buffer->resource.size;
            if (!(bo->shadow = calloc(1, (bo->size + 3) & ~(size_t)3)))
            {
                free(bo);
                return FALSE;
            }
            list_init(&bo->b.users);
            bo->b.refcount = 1;
            bo->b.coherent = true;
            bo->id = wgpu_alloc_id();
            if (buffer->resource.bind_flags & WINED3D_BIND_VERTEX_BUFFER)
                usage |= D3DGPU_BUFFER_VERTEX;
            if (buffer->resource.bind_flags & WINED3D_BIND_INDEX_BUFFER)
                usage |= D3DGPU_BUFFER_INDEX;
            if (buffer->resource.usage & WINED3DUSAGE_DYNAMIC)
                usage |= D3DGPU_BUFFER_DYNAMIC;
            if ((cmd = wgpu_cmd(device, D3DGPU_OP_CREATE_BUFFER, sizeof(*cmd))))
            {
                cmd->id = bo->id;
                cmd->size = (bo->size + 3) & ~(size_t)3;
                cmd->usage = usage;
            }
            buffer->buffer_object = &bo->b;
            return TRUE;

        default:
            FIXME("Unhandled location %s.\n", wined3d_debug_location(location));
            return FALSE;
    }
}

void wined3d_buffer_wgpu_unload_location(struct wined3d_buffer *buffer,
        struct wined3d_context *context, unsigned int location)
{
    struct wined3d_device_wgpu *device = wined3d_device_wgpu(buffer->resource.device);
    struct wined3d_bo_wgpu *bo;

    TRACE("buffer %p, context %p, location %s.\n", buffer, context, wined3d_debug_location(location));

    if (location != WINED3D_LOCATION_BUFFER || !buffer->buffer_object)
        return;
    bo = wined3d_bo_wgpu(buffer->buffer_object);
    if (buffer->bo_user.valid)
    {
        buffer->bo_user.valid = false;
        list_remove(&buffer->bo_user.entry);
    }
    if (!--bo->b.refcount)
    {
        wgpu_destroy_object(device, bo->id);
        free(bo->shadow);
        free(bo);
    }
    buffer->buffer_object = NULL;
}

static void *adapter_wgpu_map_bo_address(struct wined3d_context *context,
        const struct wined3d_bo_address *data, size_t size, uint32_t map_flags)
{
    if (!data->buffer_object)
        return data->addr;
    return wined3d_bo_wgpu(data->buffer_object)->shadow + (uintptr_t)data->addr;
}

static void adapter_wgpu_unmap_bo_address(struct wined3d_context *context,
        const struct wined3d_bo_address *data, unsigned int range_count, const struct wined3d_range *ranges)
{
    struct wined3d_device_wgpu *device = wined3d_device_wgpu(context->device);
    struct wined3d_bo_wgpu *bo;
    unsigned int i;

    if (!data->buffer_object)
        return;
    bo = wined3d_bo_wgpu(data->buffer_object);
    for (i = 0; i < range_count; ++i)
        wgpu_write_bo(device, bo, (uintptr_t)data->addr + ranges[i].offset, ranges[i].size);
}

static void adapter_wgpu_copy_bo_address(struct wined3d_context *context,
        const struct wined3d_bo_address *dst, const struct wined3d_bo_address *src,
        unsigned int range_count, const struct wined3d_range *ranges, uint32_t map_flags)
{
    struct wined3d_device_wgpu *device = wined3d_device_wgpu(context->device);
    BYTE *dst_ptr, *src_ptr;
    unsigned int i;

    dst_ptr = adapter_wgpu_map_bo_address(context, dst, 0, map_flags);
    src_ptr = adapter_wgpu_map_bo_address(context, src, 0, WINED3D_MAP_READ);
    for (i = 0; i < range_count; ++i)
        memcpy(dst_ptr + ranges[i].offset, src_ptr + ranges[i].offset, ranges[i].size);
    if (dst->buffer_object)
    {
        for (i = 0; i < range_count; ++i)
            wgpu_write_bo(device, wined3d_bo_wgpu(dst->buffer_object),
                    (uintptr_t)dst->addr + ranges[i].offset, ranges[i].size);
    }
}

static void adapter_wgpu_flush_bo_address(struct wined3d_context *context,
        const struct wined3d_const_bo_address *data, size_t size)
{
    struct wined3d_device_wgpu *device = wined3d_device_wgpu(context->device);

    if (data->buffer_object)
        wgpu_write_bo(device, wined3d_bo_wgpu(data->buffer_object), (uintptr_t)data->addr, size);
}

static bool adapter_wgpu_alloc_bo(struct wined3d_device *device, struct wined3d_resource *resource,
        unsigned int sub_resource_idx, struct wined3d_bo_address *addr)
{
    return false;
}

static void adapter_wgpu_destroy_bo(struct wined3d_context *context, struct wined3d_bo *bo)
{
    struct wined3d_bo_wgpu *bo_wgpu = wined3d_bo_wgpu(bo);

    wgpu_destroy_object(wined3d_device_wgpu(context->device), bo_wgpu->id);
    free(bo_wgpu->shadow);
    free(bo_wgpu);
}

/* The bytes of a buffer as the GPU will see them. */
static const BYTE *wgpu_buffer_data(struct wined3d_buffer *buffer, struct wined3d_context *context)
{
    struct wined3d_bo_address addr;

    if (!wined3d_buffer_get_memory(buffer, context, &addr))
        return NULL;
    if (addr.buffer_object)
        return wined3d_bo_wgpu(addr.buffer_object)->shadow + (uintptr_t)addr.addr;
    return addr.addr;
}

/* ---- Textures ---------------------------------------------------------- */

struct wined3d_texture_wgpu
{
    struct wined3d_texture t;
    uint32_t id;
};

static inline struct wined3d_texture_wgpu *wined3d_texture_wgpu(struct wined3d_texture *texture)
{
    return CONTAINING_RECORD(texture, struct wined3d_texture_wgpu, t);
}

static void wgpu_sub_resource_region(struct wined3d_texture *texture, unsigned int sub_resource_idx,
        const struct wined3d_box *box, struct d3dgpu_texture_region *region)
{
    unsigned int level = sub_resource_idx % texture->level_count;

    region->texture = wined3d_texture_wgpu(texture)->id;
    region->face = sub_resource_idx / texture->level_count;
    region->level = level;
    if (box)
    {
        region->x = box->left;
        region->y = box->top;
        region->z = box->front;
        region->width = box->right - box->left;
        region->height = box->bottom - box->top;
        region->depth = box->back - box->front;
    }
    else
    {
        region->x = region->y = region->z = 0;
        region->width = wined3d_texture_get_level_width(texture, level);
        region->height = wined3d_texture_get_level_height(texture, level);
        region->depth = wined3d_texture_get_level_depth(texture, level);
    }
}

static BOOL wgpu_prepare_texture(struct wined3d_texture *texture)
{
    struct wined3d_device_wgpu *device = wined3d_device_wgpu(texture->resource.device);
    struct wined3d_texture_wgpu *texture_wgpu = wined3d_texture_wgpu(texture);
    struct wined3d_resource *resource = &texture->resource;
    struct d3dgpu_cmd_create_texture *cmd;
    uint32_t format;

    if (texture_wgpu->id)
        return TRUE;
    if (!(format = d3dformat_from_wined3d(resource->format->id)))
    {
        FIXME("Unsupported texture format %s.\n", debug_d3dformat(resource->format->id));
        return FALSE;
    }
    if (!(cmd = wgpu_cmd(device, D3DGPU_OP_CREATE_TEXTURE, sizeof(*cmd))))
        return FALSE;
    texture_wgpu->id = wgpu_alloc_id();
    cmd->id = texture_wgpu->id;
    if (resource->type == WINED3D_RTYPE_TEXTURE_3D)
        cmd->kind = D3DGPU_TEXTURE_VOLUME;
    else if (resource->usage & WINED3DUSAGE_LEGACY_CUBEMAP)
        cmd->kind = D3DGPU_TEXTURE_CUBE;
    else
        cmd->kind = D3DGPU_TEXTURE_2D;
    cmd->format = format;
    cmd->width = resource->width;
    cmd->height = resource->height;
    cmd->depth = resource->depth;
    cmd->levels = texture->level_count;
    if (resource->bind_flags & WINED3D_BIND_RENDER_TARGET)
        cmd->usage |= D3DGPU_TEXTURE_RENDER_TARGET;
    if (resource->bind_flags & WINED3D_BIND_DEPTH_STENCIL)
        cmd->usage |= D3DGPU_TEXTURE_DEPTH_STENCIL;
    if (resource->usage & WINED3DUSAGE_DYNAMIC)
        cmd->usage |= D3DGPU_TEXTURE_DYNAMIC;
    return TRUE;
}

static void wgpu_upload(struct wined3d_device_wgpu *device, const BYTE *src, unsigned int row_pitch,
        unsigned int slice_pitch, struct wined3d_texture *texture, unsigned int sub_resource_idx,
        const struct wined3d_box *box)
{
    struct d3dgpu_cmd_write_texture *cmd;
    struct d3dgpu_texture_region region;
    unsigned int size;

    wgpu_sub_resource_region(texture, sub_resource_idx, box, &region);
    size = slice_pitch * region.depth;
    if (!(cmd = wgpu_cmd_data(device, D3DGPU_OP_WRITE_TEXTURE, sizeof(*cmd), src, size)))
        return;
    cmd->region = region;
    cmd->row_pitch = row_pitch;
    cmd->slice_pitch = slice_pitch;
}

/* Reads a region back into `dst`, in chunks of whole rows that fit the
 * readback region. */
static void wgpu_download(struct wined3d_device_wgpu *device, struct wined3d_texture *texture,
        unsigned int sub_resource_idx, const struct wined3d_box *box, BYTE *dst,
        unsigned int row_pitch, unsigned int slice_pitch)
{
    const struct wined3d_format *format = texture->resource.format;
    struct d3dgpu_texture_region region, part;
    unsigned int z, y, rows, block_h;
    struct d3dgpu_cmd_read_texture *cmd;

    wgpu_sub_resource_region(texture, sub_resource_idx, box, &region);
    block_h = (format->attrs & WINED3D_FORMAT_ATTR_BLOCKS) ? format->block_height : 1;
    rows = max(block_h, (wgpu_host.shared_size / row_pitch) / block_h * block_h);
    for (z = 0; z < region.depth; ++z)
    {
        for (y = 0; y < region.height; y += rows)
        {
            part = region;
            part.z = region.z + z;
            part.depth = 1;
            part.y = region.y + y;
            part.height = min(rows, region.height - y);
            if (!(cmd = wgpu_cmd(device, D3DGPU_OP_READ_TEXTURE, sizeof(*cmd))))
                return;
            cmd->region = part;
            cmd->dest_offset = 0;
            cmd->row_pitch = row_pitch;
            cmd->slice_pitch = row_pitch * ((part.height + block_h - 1) / block_h);
            cmd->fence_lo = device->fence + 1;
            cmd->fence_hi = (device->fence + 1) >> 32;
            wgpu_wait(device, 0, cmd->slice_pitch, dst + z * slice_pitch + (y / block_h) * row_pitch);
        }
    }
}

static void wined3d_texture_wgpu_upload_data(struct wined3d_context *context,
        const struct wined3d_const_bo_address *src_bo_addr, const struct wined3d_format *src_format,
        const struct wined3d_box *src_box, unsigned int src_row_pitch, unsigned int src_slice_pitch,
        struct wined3d_texture *dst_texture, unsigned int dst_sub_resource_idx, unsigned int dst_location,
        unsigned int dst_x, unsigned int dst_y, unsigned int dst_z)
{
    struct wined3d_device_wgpu *device = wined3d_device_wgpu(context->device);
    struct wined3d_box dst_box;
    const BYTE *src;

    TRACE("context %p, src_bo_addr %s, src_format %s, src_box %s, src_row_pitch %u, src_slice_pitch %u, "
            "dst_texture %p, dst_sub_resource_idx %u, dst_location %s, dst_x %u, dst_y %u, dst_z %u.\n",
            context, debug_const_bo_address(src_bo_addr), debug_d3dformat(src_format->id), debug_box(src_box),
            src_row_pitch, src_slice_pitch, dst_texture, dst_sub_resource_idx,
            wined3d_debug_location(dst_location), dst_x, dst_y, dst_z);

    if (src_format->id != dst_texture->resource.format->id)
        FIXME("Format conversion %s -> %s not supported.\n",
                debug_d3dformat(src_format->id), debug_d3dformat(dst_texture->resource.format->id));
    if (!wgpu_prepare_texture(dst_texture))
        return;
    src = src_bo_addr->buffer_object
            ? wined3d_bo_wgpu(src_bo_addr->buffer_object)->shadow + (uintptr_t)src_bo_addr->addr
            : src_bo_addr->addr;
    src += src_box->front * src_slice_pitch;
    if (src_format->attrs & WINED3D_FORMAT_ATTR_BLOCKS)
        src += (src_box->top / src_format->block_height) * src_row_pitch
                + (src_box->left / src_format->block_width) * src_format->block_byte_count;
    else
        src += src_box->top * src_row_pitch + src_box->left * src_format->byte_count;
    wined3d_box_set(&dst_box, dst_x, dst_y, dst_x + src_box->right - src_box->left,
            dst_y + src_box->bottom - src_box->top, dst_z, dst_z + src_box->back - src_box->front);
    wgpu_upload(device, src, src_row_pitch, src_slice_pitch, dst_texture, dst_sub_resource_idx, &dst_box);
}

static void wined3d_texture_wgpu_download_data(struct wined3d_context *context,
        struct wined3d_texture *src_texture, unsigned int src_sub_resource_idx, unsigned int src_location,
        const struct wined3d_box *src_box, const struct wined3d_bo_address *dst_bo_addr,
        const struct wined3d_format *dst_format, unsigned int dst_x, unsigned int dst_y, unsigned int dst_z,
        unsigned int dst_row_pitch, unsigned int dst_slice_pitch)
{
    struct wined3d_device_wgpu *device = wined3d_device_wgpu(context->device);
    BYTE *dst;

    TRACE("context %p, src_texture %p, src_sub_resource_idx %u, src_location %s, src_box %s, dst_bo_addr %s, "
            "dst_format %s, dst_x %u, dst_y %u, dst_z %u, dst_row_pitch %u, dst_slice_pitch %u.\n",
            context, src_texture, src_sub_resource_idx, wined3d_debug_location(src_location),
            debug_box(src_box), debug_bo_address(dst_bo_addr), debug_d3dformat(dst_format->id),
            dst_x, dst_y, dst_z, dst_row_pitch, dst_slice_pitch);

    if (!wined3d_texture_wgpu(src_texture)->id)
        return;
    dst = adapter_wgpu_map_bo_address(context, dst_bo_addr, 0, WINED3D_MAP_WRITE);
    dst += dst_z * dst_slice_pitch;
    if (dst_format->attrs & WINED3D_FORMAT_ATTR_BLOCKS)
        dst += (dst_y / dst_format->block_height) * dst_row_pitch
                + (dst_x / dst_format->block_width) * dst_format->block_byte_count;
    else
        dst += dst_y * dst_row_pitch + dst_x * dst_format->byte_count;
    wgpu_download(device, src_texture, src_sub_resource_idx, src_box, dst, dst_row_pitch, dst_slice_pitch);
}

static void wgpu_clear_sub_resource(struct wined3d_device_wgpu *device, struct wined3d_texture *texture,
        unsigned int sub_resource_idx)
{
    const struct wined3d_texture_sub_resource *sub_resource = &texture->sub_resources[sub_resource_idx];
    bool depth = texture->resource.format->depth_size || texture->resource.format->stencil_size;
    struct d3dgpu_cmd_set_render_target *rt;
    struct d3dgpu_cmd_set_depth_stencil *ds;
    struct d3dgpu_cmd_clear *clear;
    unsigned int i;

    /* Clear through a render target binding, then restore no binding: the
     * next draw sets its own. */
    if (depth)
    {
        if ((ds = wgpu_cmd(device, D3DGPU_OP_SET_DEPTH_STENCIL, sizeof(*ds))))
        {
            ds->texture = wined3d_texture_wgpu(texture)->id;
            ds->face = sub_resource_idx / texture->level_count;
            ds->level = sub_resource_idx % texture->level_count;
        }
        for (i = 0; i < 4; ++i)
            if ((rt = wgpu_cmd(device, D3DGPU_OP_SET_RENDER_TARGET, sizeof(*rt))))
                rt->index = i;
    }
    else
    {
        if ((rt = wgpu_cmd(device, D3DGPU_OP_SET_RENDER_TARGET, sizeof(*rt))))
        {
            rt->index = 0;
            rt->texture = wined3d_texture_wgpu(texture)->id;
            rt->face = sub_resource_idx / texture->level_count;
            rt->level = sub_resource_idx % texture->level_count;
        }
        for (i = 1; i < 4; ++i)
            if ((rt = wgpu_cmd(device, D3DGPU_OP_SET_RENDER_TARGET, sizeof(*rt))))
                rt->index = i;
        wgpu_cmd(device, D3DGPU_OP_SET_DEPTH_STENCIL, sizeof(*ds));
    }
    wgpu_full_viewport(device, wined3d_texture_get_level_width(texture, sub_resource_idx % texture->level_count),
            wined3d_texture_get_level_height(texture, sub_resource_idx % texture->level_count));
    if ((clear = wgpu_cmd(device, D3DGPU_OP_CLEAR, sizeof(*clear))))
    {
        if (depth)
        {
            clear->flags = (texture->resource.format->depth_size ? WINED3DCLEAR_ZBUFFER : 0)
                    | (texture->resource.format->stencil_size ? WINED3DCLEAR_STENCIL : 0);
            clear->z = sub_resource->clear_value.depth;
            clear->stencil = sub_resource->clear_value.stencil;
        }
        else
        {
            clear->flags = WINED3DCLEAR_TARGET;
            clear->color = d3dcolor_from_wined3d(&sub_resource->clear_value.colour);
        }
    }
    device->decl_id = ~0u; /* force the next draw to rebind everything */
}

static BOOL wined3d_texture_wgpu_prepare_location(struct wined3d_texture *texture,
        unsigned int sub_resource_idx, struct wined3d_context *context, unsigned int location)
{
    switch (location)
    {
        case WINED3D_LOCATION_SYSMEM:
            return texture->sub_resources[sub_resource_idx].user_memory ? TRUE
                    : wined3d_resource_prepare_sysmem(&texture->resource);

        case WINED3D_LOCATION_TEXTURE_RGB:
            return wgpu_prepare_texture(texture);

        case WINED3D_LOCATION_DRAWABLE:
            return TRUE;

        default:
            FIXME("Unhandled location %s.\n", wined3d_debug_location(location));
            return FALSE;
    }
}

static BOOL wined3d_texture_wgpu_load_location(struct wined3d_texture *texture,
        unsigned int sub_resource_idx, struct wined3d_context *context, uint32_t location)
{
    struct wined3d_device_wgpu *device = wined3d_device_wgpu(context->device);
    struct wined3d_texture_sub_resource *sub_resource = &texture->sub_resources[sub_resource_idx];
    unsigned int level = sub_resource_idx % texture->level_count;
    unsigned int row_pitch, slice_pitch;
    struct wined3d_bo_address data;

    TRACE("texture %p, sub_resource_idx %u, context %p, location %s (have %s).\n", texture, sub_resource_idx,
            context, wined3d_debug_location(location), wined3d_debug_location(sub_resource->locations));

    if (!wined3d_texture_wgpu_prepare_location(texture, sub_resource_idx, context, location))
        return FALSE;
    wined3d_texture_get_pitch(texture, level, &row_pitch, &slice_pitch);

    switch (location)
    {
        case WINED3D_LOCATION_TEXTURE_RGB:
            if (sub_resource->locations & WINED3D_LOCATION_CLEARED)
            {
                wgpu_clear_sub_resource(device, texture, sub_resource_idx);
                return TRUE;
            }
            if (!(sub_resource->locations & WINED3D_LOCATION_SYSMEM))
            {
                ERR("Unimplemented load from %s.\n", wined3d_debug_location(sub_resource->locations));
                return FALSE;
            }
            wined3d_texture_get_memory(texture, sub_resource_idx, context, &data);
            wgpu_upload(device, data.addr, row_pitch, slice_pitch, texture, sub_resource_idx, NULL);
            return TRUE;

        case WINED3D_LOCATION_SYSMEM:
            if (!(sub_resource->locations & WINED3D_LOCATION_TEXTURE_RGB))
            {
                if (sub_resource->locations & WINED3D_LOCATION_DRAWABLE)
                    return TRUE; /* The front buffer: nothing readable. */
                ERR("Unimplemented load from %s.\n", wined3d_debug_location(sub_resource->locations));
                return FALSE;
            }
            wined3d_texture_get_bo_address(texture, sub_resource_idx, &data, WINED3D_LOCATION_SYSMEM);
            wgpu_download(device, texture, sub_resource_idx, NULL, data.addr, row_pitch, slice_pitch);
            return TRUE;

        case WINED3D_LOCATION_DRAWABLE:
            return TRUE;

        default:
            FIXME("Unimplemented location %s.\n", wined3d_debug_location(location));
            return FALSE;
    }
}

static void wined3d_texture_wgpu_unload_location(struct wined3d_texture *texture,
        struct wined3d_context *context, unsigned int location)
{
    struct wined3d_texture_wgpu *texture_wgpu = wined3d_texture_wgpu(texture);

    TRACE("texture %p, context %p, location %s.\n", texture, context, wined3d_debug_location(location));

    if (location == WINED3D_LOCATION_TEXTURE_RGB && texture_wgpu->id)
    {
        wgpu_destroy_object(wined3d_device_wgpu(texture->resource.device), texture_wgpu->id);
        texture_wgpu->id = 0;
    }
}

static const struct wined3d_texture_ops wined3d_texture_wgpu_ops =
{
    wined3d_texture_wgpu_prepare_location,
    wined3d_texture_wgpu_load_location,
    wined3d_texture_wgpu_unload_location,
    wined3d_texture_wgpu_upload_data,
    wined3d_texture_wgpu_download_data,
};

/* ---- Shaders ----------------------------------------------------------- */

struct wgpu_shader_priv
{
    const struct wined3d_vertex_pipe_ops *vertex_pipe;
    const struct wined3d_fragment_pipe_ops *fragment_pipe;
};

static void shader_wgpu_handle_instruction(const struct wined3d_shader_instruction *ins) {}
static void shader_wgpu_precompile(void *shader_priv, struct wined3d_shader *shader) {}
static void shader_wgpu_apply_draw_state(void *shader_priv, struct wined3d_context *context,
        const struct wined3d_state *state) {}
static void shader_wgpu_apply_compute_state(void *shader_priv, struct wined3d_context *context,
        const struct wined3d_state *state) {}
static void shader_wgpu_disable(void *shader_priv, struct wined3d_context *context)
{
    context->shader_update_mask = (1u << WINED3D_SHADER_TYPE_PIXEL)
            | (1u << WINED3D_SHADER_TYPE_VERTEX)
            | (1u << WINED3D_SHADER_TYPE_GEOMETRY)
            | (1u << WINED3D_SHADER_TYPE_HULL)
            | (1u << WINED3D_SHADER_TYPE_DOMAIN)
            | (1u << WINED3D_SHADER_TYPE_COMPUTE);
}
static void shader_wgpu_update_float_vertex_constants(struct wined3d_device *device, UINT start, UINT count) {}
static void shader_wgpu_update_float_pixel_constants(struct wined3d_device *device, UINT start, UINT count) {}

static void shader_wgpu_destroy(struct wined3d_shader *shader)
{
    struct wined3d_device_wgpu *device = wined3d_device_wgpu(shader->device);
    uint32_t id = (uint32_t)(ULONG_PTR)shader->backend_data;

    if (!id)
        return;
    if (device->vs_id == id)
        device->vs_id = 0;
    if (device->ps_id == id)
        device->ps_id = 0;
    wgpu_destroy_object(device, id);
    shader->backend_data = NULL;
}

static HRESULT shader_wgpu_alloc(struct wined3d_device *device, const struct wined3d_vertex_pipe_ops *vertex_pipe,
        const struct wined3d_fragment_pipe_ops *fragment_pipe);
static void shader_wgpu_free(struct wined3d_device *device, struct wined3d_context *context);

static BOOL shader_wgpu_allocate_context_data(struct wined3d_context *context)
{
    return TRUE;
}

static void shader_wgpu_free_context_data(struct wined3d_context *context) {}
static void shader_wgpu_init_context_state(struct wined3d_context *context) {}

static void shader_wgpu_get_caps(const struct wined3d_adapter *adapter, struct shader_caps *caps)
{
    memset(caps, 0, sizeof(*caps));
    /* Direct3D 9: shader model 3 for vertex and pixel shaders. */
    caps->vs_version = min(wined3d_settings.max_sm_vs, 3);
    caps->ps_version = min(wined3d_settings.max_sm_ps, 3);
    caps->vs_uniform_count = WINED3D_MAX_VS_CONSTS_F;
    caps->ps_uniform_count = WINED3D_MAX_PS_CONSTS_F;
    caps->ps_1x_max_value = FLT_MAX;
    caps->varying_count = 0;
    caps->wined3d_caps = WINED3D_SHADER_CAP_FULL_FFP_VARYINGS;
}

static BOOL shader_wgpu_color_fixup_supported(struct color_fixup_desc fixup)
{
    return is_identity_fixup(fixup);
}

static uint64_t shader_wgpu_shader_compile(struct wined3d_context *context, const struct wined3d_shader_desc *shader_desc,
        enum wined3d_shader_type shader_type)
{
    return 0;
}

static const struct wined3d_shader_backend_ops wgpu_shader_backend =
{
    shader_wgpu_handle_instruction,
    shader_wgpu_precompile,
    shader_wgpu_apply_draw_state,
    shader_wgpu_apply_compute_state,
    shader_wgpu_disable,
    shader_wgpu_update_float_vertex_constants,
    shader_wgpu_update_float_pixel_constants,
    shader_wgpu_destroy,
    shader_wgpu_alloc,
    shader_wgpu_free,
    shader_wgpu_allocate_context_data,
    shader_wgpu_free_context_data,
    shader_wgpu_init_context_state,
    shader_wgpu_get_caps,
    shader_wgpu_color_fixup_supported,
    shader_wgpu_shader_compile,
};

static HRESULT shader_wgpu_alloc(struct wined3d_device *device, const struct wined3d_vertex_pipe_ops *vertex_pipe,
        const struct wined3d_fragment_pipe_ops *fragment_pipe)
{
    struct wgpu_shader_priv *priv;
    void *vertex_priv, *fragment_priv;

    if (!(priv = calloc(1, sizeof(*priv))))
        return E_OUTOFMEMORY;
    if (!(vertex_priv = vertex_pipe->vp_alloc(&wgpu_shader_backend, priv)))
    {
        free(priv);
        return E_FAIL;
    }
    if (!(fragment_priv = fragment_pipe->alloc_private(&wgpu_shader_backend, priv)))
    {
        vertex_pipe->vp_free(device, NULL);
        free(priv);
        return E_FAIL;
    }
    priv->vertex_pipe = vertex_pipe;
    priv->fragment_pipe = fragment_pipe;
    device->vertex_priv = vertex_priv;
    device->fragment_priv = fragment_priv;
    device->shader_priv = priv;
    return WINED3D_OK;
}

static void shader_wgpu_free(struct wined3d_device *device, struct wined3d_context *context)
{
    struct wgpu_shader_priv *priv = device->shader_priv;

    priv->fragment_pipe->free_private(device, context);
    priv->vertex_pipe->vp_free(device, context);
    free(priv);
}

/* The fixed-function pipeline is HLSL compiled to shaders (ffp_hlsl), so
 * these pipes only report caps. */
static void wgpu_vp_apply_draw_state(struct wined3d_context *context, const struct wined3d_state *state) {}
static void wgpu_vp_disable(const struct wined3d_context *context) {}

static void wgpu_vp_get_caps(const struct wined3d_adapter *adapter, struct wined3d_vertex_caps *caps)
{
    memset(caps, 0, sizeof(*caps));
    caps->emulated_flatshading = true;
}

static unsigned int wgpu_vp_get_emul_mask(const struct wined3d_adapter *adapter)
{
    return 0;
}

static void *wgpu_vp_alloc(const struct wined3d_shader_backend_ops *shader_backend, void *shader_priv)
{
    return shader_priv;
}

static void wgpu_vp_free(struct wined3d_device *device, struct wined3d_context *context) {}

static const struct wined3d_state_entry_template wgpu_vp_states[] =
{
    {STATE_SHADER(WINED3D_SHADER_TYPE_VERTEX), {STATE_SHADER(WINED3D_SHADER_TYPE_VERTEX), state_nop}},
    {0},
};

static const struct wined3d_vertex_pipe_ops wgpu_vertex_pipe =
{
    .vp_apply_draw_state = wgpu_vp_apply_draw_state,
    .vp_disable = wgpu_vp_disable,
    .vp_get_caps = wgpu_vp_get_caps,
    .vp_get_emul_mask = wgpu_vp_get_emul_mask,
    .vp_alloc = wgpu_vp_alloc,
    .vp_free = wgpu_vp_free,
    .vp_states = wgpu_vp_states,
};

static void wgpu_fp_apply_draw_state(struct wined3d_context *context, const struct wined3d_state *state) {}
static void wgpu_fp_disable(const struct wined3d_context *context) {}

static void wgpu_fp_get_caps(const struct wined3d_adapter *adapter, struct fragment_caps *caps)
{
    memset(caps, 0, sizeof(*caps));
    caps->max_blend_stages = WINED3D_MAX_FFP_TEXTURES;
}

static unsigned int wgpu_fp_get_emul_mask(const struct wined3d_adapter *adapter)
{
    return 0;
}

static void *wgpu_fp_alloc(const struct wined3d_shader_backend_ops *shader_backend, void *shader_priv)
{
    return shader_priv;
}

static void wgpu_fp_free(struct wined3d_device *device, struct wined3d_context *context) {}

static BOOL wgpu_fp_alloc_context_data(struct wined3d_context *context)
{
    return TRUE;
}

static void wgpu_fp_free_context_data(struct wined3d_context *context) {}

static const struct wined3d_state_entry_template wgpu_fp_states[] =
{
    {STATE_RENDER(WINED3D_RS_FOGVERTEXMODE), {STATE_RENDER(WINED3D_RS_FOGVERTEXMODE), state_nop}},
    {0},
};

static const struct wined3d_fragment_pipe_ops wgpu_fragment_pipe =
{
    .fp_apply_draw_state = wgpu_fp_apply_draw_state,
    .fp_disable = wgpu_fp_disable,
    .get_caps = wgpu_fp_get_caps,
    .get_emul_mask = wgpu_fp_get_emul_mask,
    .alloc_private = wgpu_fp_alloc,
    .free_private = wgpu_fp_free,
    .allocate_context_data = wgpu_fp_alloc_context_data,
    .free_context_data = wgpu_fp_free_context_data,
    .color_fixup_supported = shader_wgpu_color_fixup_supported,
    .states = wgpu_fp_states,
};

/* The core's shader for a wined3d shader, created on first use. */
static uint32_t wgpu_shader_id(struct wined3d_device_wgpu *device, struct wined3d_shader *shader)
{
    struct d3dgpu_cmd_create_shader *cmd;
    uint32_t id;

    if (!shader)
        return 0;
    if ((id = (uint32_t)(ULONG_PTR)shader->backend_data))
        return id;
    if (shader->reg_maps.shader_version.major >= 4)
    {
        FIXME("Shader model %u shaders are not supported yet.\n", shader->reg_maps.shader_version.major);
        return 0;
    }
    if (!(cmd = wgpu_cmd_data(device, D3DGPU_OP_CREATE_SHADER, sizeof(*cmd),
            shader->byte_code, shader->byte_code_size)))
        return 0;
    id = wgpu_alloc_id();
    cmd->id = id;
    cmd->stage = shader->reg_maps.shader_version.type == WINED3D_SHADER_TYPE_PIXEL
            ? D3DGPU_STAGE_PIXEL : D3DGPU_STAGE_VERTEX;
    /* The core caches translations by this hash (FNV-1a over the bytecode). */
    {
        const BYTE *b = shader->byte_code;
        uint64_t h = 0xcbf29ce484222325ull;
        unsigned int i;

        for (i = 0; i < shader->byte_code_size; ++i)
            h = (h ^ b[i]) * 0x100000001b3ull;
        h |= 1;
        cmd->hash_lo = h;
        cmd->hash_hi = h >> 32;
    }
    shader->backend_data = (void *)(ULONG_PTR)id;
    return id;
}

/* ---- Draws ------------------------------------------------------------- */

static uint32_t wgpu_decl_hash(const struct wined3d_vertex_declaration *decl)
{
    uint32_t h = 2166136261u;
    unsigned int i;

    for (i = 0; i < decl->element_count; ++i)
    {
        const struct wined3d_vertex_declaration_element *e = &decl->elements[i];
        uint32_t v[5] = {e->format->id, e->input_slot, e->offset, e->usage | e->usage_idx << 8 | e->method << 16,
                e->output_slot};
        unsigned int j;

        for (j = 0; j < ARRAY_SIZE(v); ++j)
            h = (h ^ v[j]) * 16777619u;
    }
    return h ^ decl->element_count;
}

static uint32_t wgpu_vertex_decl_id(struct wined3d_device_wgpu *device, const struct wined3d_vertex_declaration *decl)
{
    struct d3dgpu_cmd_create_vertex_decl *cmd;
    uint32_t hash = wgpu_decl_hash(decl);
    struct wgpu_vertex_decl *entry = NULL;
    unsigned int i, count = 0;
    BYTE *elements;

    for (i = 0; i < device->decl_count; ++i)
    {
        if (device->decls[i].decl == decl)
        {
            if (device->decls[i].hash == hash)
                return device->decls[i].id;
            entry = &device->decls[i];
            wgpu_destroy_object(device, entry->id);
            break;
        }
    }
    if (!entry)
    {
        if (!wined3d_array_reserve((void **)&device->decls, &device->decls_size,
                device->decl_count + 1, sizeof(*device->decls)))
            return 0;
        entry = &device->decls[device->decl_count++];
    }

    if (!(cmd = wgpu_cmd(device, D3DGPU_OP_CREATE_VERTEX_DECL, sizeof(*cmd) + decl->element_count * 8)))
        return 0;
    elements = (BYTE *)(cmd + 1);
    for (i = 0; i < decl->element_count; ++i)
    {
        const struct wined3d_vertex_declaration_element *e = &decl->elements[i];
        uint32_t type = decl_type_from_wined3d(e->format->id);
        BYTE *d = elements + count * 8;

        if (type == ~0u)
        {
            FIXME("Unsupported vertex element format %s.\n", debug_d3dformat(e->format->id));
            continue;
        }
        /* D3DVERTEXELEMENT9: Stream, Offset (WORD), Type, Method, Usage, UsageIndex. */
        d[0] = e->input_slot;
        d[1] = e->input_slot >> 8;
        d[2] = e->offset;
        d[3] = e->offset >> 8;
        d[4] = type;
        d[5] = e->method;
        /* Pre-transformed positions go through wined3d's fixed-function
         * vertex shader too, which reads them as POSITION. */
        d[6] = e->usage == WINED3D_DECL_USAGE_POSITIONT ? WINED3D_DECL_USAGE_POSITION : e->usage;
        d[7] = e->usage_idx;
        ++count;
    }
    cmd->h.size = sizeof(*cmd) + count * 8;
    device->cmd_size -= (decl->element_count - count) * 8;
    entry->decl = decl;
    entry->hash = hash;
    entry->id = wgpu_alloc_id();
    cmd->id = entry->id;
    cmd->count = count;
    return entry->id;
}

static void wgpu_forget_decl(struct wined3d_device_wgpu *device, const struct wined3d_vertex_declaration *decl)
{
    unsigned int i;

    for (i = 0; i < device->decl_count; ++i)
    {
        if (device->decls[i].decl == decl)
        {
            wgpu_destroy_object(device, device->decls[i].id);
            device->decls[i] = device->decls[--device->decl_count];
            return;
        }
    }
}

/* Sends the shader constants that changed since the last draw. */
static void wgpu_upload_constants(struct wined3d_device_wgpu *device, struct wined3d_context *context,
        const struct wined3d_state *state, enum wined3d_shader_type type)
{
    unsigned int stage = type == WINED3D_SHADER_TYPE_PIXEL ? D3DGPU_STAGE_PIXEL : D3DGPU_STAGE_VERTEX;
    const struct wined3d_constant_buffer_state *cb = state->cb[type];
    struct d3dgpu_cmd_set_shader_const *cmd;
    unsigned int i, count, start, end;
    const BYTE *data;

    /* Float constants. */
    if (cb[0].buffer && (data = wgpu_buffer_data(cb[0].buffer, context)))
    {
        const float (*f)[4] = (const float (*)[4])(data + cb[0].offset);

        count = min(cb[0].size ? cb[0].size : cb[0].buffer->resource.size, cb[0].buffer->resource.size - cb[0].offset);
        count = min(count / 16, WINED3D_MAX_VS_CONSTS_F);
        for (start = 0; start < count; start = end)
        {
            while (start < count && device->consts_valid && start < device->consts_f_count[stage]
                    && !memcmp(f[start], device->consts_f[stage][start], 16))
                ++start;
            if (start == count)
                break;
            end = start + 1;
            while (end < count && (!device->consts_valid || end >= device->consts_f_count[stage]
                    || memcmp(f[end], device->consts_f[stage][end], 16)))
                ++end;
            if ((cmd = wgpu_cmd(device, D3DGPU_OP_SET_SHADER_CONST_F, sizeof(*cmd) + (end - start) * 16)))
            {
                cmd->stage = stage;
                cmd->start = start;
                cmd->count = end - start;
                memcpy(cmd + 1, f[start], (end - start) * 16);
            }
            memcpy(device->consts_f[stage][start], f[start], (end - start) * 16);
        }
        device->consts_f_count[stage] = max(device->consts_f_count[stage], count);
    }

    /* Integer constants. */
    if (cb[1].buffer && (data = wgpu_buffer_data(cb[1].buffer, context)))
    {
        data += cb[1].offset;
        count = min(cb[1].buffer->resource.size - cb[1].offset, sizeof(device->consts_i[stage]));
        if (!device->consts_valid || memcmp(data, device->consts_i[stage], count))
        {
            if ((cmd = wgpu_cmd(device, D3DGPU_OP_SET_SHADER_CONST_I, sizeof(*cmd) + count)))
            {
                cmd->stage = stage;
                cmd->start = 0;
                cmd->count = count / 16;
                memcpy(cmd + 1, data, count);
            }
            memcpy(device->consts_i[stage], data, count);
        }
    }

    /* Boolean constants, one 32-bit value each. */
    if (cb[2].buffer && (data = wgpu_buffer_data(cb[2].buffer, context)))
    {
        data += cb[2].offset;
        count = min(cb[2].buffer->resource.size - cb[2].offset, sizeof(device->consts_b[stage]));
        if (!device->consts_valid || memcmp(data, device->consts_b[stage], count))
        {
            if ((cmd = wgpu_cmd(device, D3DGPU_OP_SET_SHADER_CONST_B, sizeof(*cmd) + count)))
            {
                cmd->stage = stage;
                cmd->start = 0;
                cmd->count = count / 4;
                memcpy(cmd + 1, data, count);
            }
            memcpy(device->consts_b[stage], data, count);
        }
    }
    (void)i;
}

static void wgpu_apply_render_states(struct wined3d_device_wgpu *device, struct wined3d_context *context,
        const struct wined3d_state *state)
{
    const struct wined3d_rasterizer_state *r = state->rasterizer_state;
    const struct wined3d_depth_stencil_state *ds = state->depth_stencil_state;
    const struct wined3d_blend_state *b = state->blend_state;
    const struct wined3d_constant_buffer_state *ps_extra = &state->cb[WINED3D_SHADER_TYPE_PIXEL][WINED3D_FFP_CONSTANTS_EXTRA_REGISTER];
    const struct wined3d_constant_buffer_state *vs_extra = &state->cb[WINED3D_SHADER_TYPE_VERTEX][WINED3D_FFP_CONSTANTS_EXTRA_REGISTER];
    const struct wined3d_ffp_ps_constants *ps_consts = NULL;
    const struct wined3d_ffp_vs_constants *vs_consts = NULL;
    const BYTE *data;
    uint32_t cull;
    unsigned int i;

    /* Blending. */
    if (b)
    {
        const struct wined3d_rendertarget_blend_state_desc *rt = &b->desc.rt[0];
        bool separate = rt->src_alpha != rt->src || rt->dst_alpha != rt->dst || rt->op_alpha != rt->op;

        wgpu_set_render_state(device, WINED3D_RS_ALPHABLENDENABLE, rt->enable);
        wgpu_set_render_state(device, WINED3D_RS_SRCBLEND, rt->src);
        wgpu_set_render_state(device, WINED3D_RS_DESTBLEND, rt->dst);
        wgpu_set_render_state(device, WINED3D_RS_BLENDOP, rt->op);
        wgpu_set_render_state(device, WINED3D_RS_SEPARATEALPHABLENDENABLE, separate);
        wgpu_set_render_state(device, WINED3D_RS_SRCBLENDALPHA, rt->src_alpha);
        wgpu_set_render_state(device, WINED3D_RS_DESTBLENDALPHA, rt->dst_alpha);
        wgpu_set_render_state(device, WINED3D_RS_BLENDOPALPHA, rt->op_alpha);
        wgpu_set_render_state(device, WINED3D_RS_COLORWRITEENABLE, rt->writemask);
        for (i = 1; i < 4; ++i)
            wgpu_set_render_state(device, WINED3D_RS_COLORWRITEENABLE1 + i - 1,
                    b->desc.independent ? b->desc.rt[i].writemask : rt->writemask);
    }
    else
    {
        wgpu_set_render_state(device, WINED3D_RS_ALPHABLENDENABLE, FALSE);
        wgpu_set_render_state(device, WINED3D_RS_COLORWRITEENABLE, 0xf);
    }
    wgpu_set_render_state(device, WINED3D_RS_BLENDFACTOR, d3dcolor_from_wined3d(&state->blend_factor));

    /* Depth and stencil. */
    if (ds)
    {
        wgpu_set_render_state(device, WINED3D_RS_ZENABLE, ds->desc.depth);
        wgpu_set_render_state(device, WINED3D_RS_ZWRITEENABLE, ds->desc.depth_write);
        wgpu_set_render_state(device, WINED3D_RS_ZFUNC, ds->desc.depth_func);
        wgpu_set_render_state(device, WINED3D_RS_STENCILENABLE, ds->desc.stencil);
        wgpu_set_render_state(device, WINED3D_RS_STENCILMASK, ds->desc.stencil_read_mask);
        wgpu_set_render_state(device, WINED3D_RS_STENCILWRITEMASK, ds->desc.stencil_write_mask);
        wgpu_set_render_state(device, WINED3D_RS_STENCILFAIL, ds->desc.front.fail_op);
        wgpu_set_render_state(device, WINED3D_RS_STENCILZFAIL, ds->desc.front.depth_fail_op);
        wgpu_set_render_state(device, WINED3D_RS_STENCILPASS, ds->desc.front.pass_op);
        wgpu_set_render_state(device, WINED3D_RS_STENCILFUNC, ds->desc.front.func);
        wgpu_set_render_state(device, WINED3D_RS_TWOSIDEDSTENCILMODE, TRUE);
        wgpu_set_render_state(device, WINED3D_RS_BACK_STENCILFAIL, ds->desc.back.fail_op);
        wgpu_set_render_state(device, WINED3D_RS_BACK_STENCILZFAIL, ds->desc.back.depth_fail_op);
        wgpu_set_render_state(device, WINED3D_RS_BACK_STENCILPASS, ds->desc.back.pass_op);
        wgpu_set_render_state(device, WINED3D_RS_BACK_STENCILFUNC, ds->desc.back.func);
    }
    else
    {
        wgpu_set_render_state(device, WINED3D_RS_ZENABLE, TRUE);
        wgpu_set_render_state(device, WINED3D_RS_ZWRITEENABLE, TRUE);
        wgpu_set_render_state(device, WINED3D_RS_ZFUNC, WINED3D_CMP_LESS);
        wgpu_set_render_state(device, WINED3D_RS_STENCILENABLE, FALSE);
    }
    wgpu_set_render_state(device, WINED3D_RS_STENCILREF, state->stencil_ref);

    /* Rasterizer. Direct3D 9 culls by winding as seen on screen. */
    if (r)
    {
        if (r->desc.cull_mode == WINED3D_CULL_NONE)
            cull = 1; /* D3DCULL_NONE */
        else if ((r->desc.cull_mode == WINED3D_CULL_BACK) != !!r->desc.front_ccw)
            cull = 3; /* D3DCULL_CCW */
        else
            cull = 2; /* D3DCULL_CW */
        wgpu_set_render_state(device, WINED3D_RS_FILLMODE, r->desc.fill_mode);
        wgpu_set_render_state(device, WINED3D_RS_CULLMODE, cull);
        wgpu_set_render_state(device, WINED3D_RS_DEPTHBIAS, float_bits(r->desc.depth_bias));
        wgpu_set_render_state(device, WINED3D_RS_SLOPESCALEDEPTHBIAS, float_bits(r->desc.scale_bias));
        wgpu_set_render_state(device, WINED3D_RS_SCISSORTESTENABLE, r->desc.scissor);
    }
    else
    {
        wgpu_set_render_state(device, WINED3D_RS_FILLMODE, WINED3D_FILL_SOLID);
        wgpu_set_render_state(device, WINED3D_RS_CULLMODE, 3);
        wgpu_set_render_state(device, WINED3D_RS_SCISSORTESTENABLE, FALSE);
    }

    /* Fixed-function states that reach the backend as shader arguments and
     * extra constants (alpha test, clip planes, sRGB writes). */
    if (ps_extra->buffer && (data = wgpu_buffer_data(ps_extra->buffer, context)))
        ps_consts = (const struct wined3d_ffp_ps_constants *)(data + ps_extra->offset);
    if (vs_extra->buffer && (data = wgpu_buffer_data(vs_extra->buffer, context)))
        vs_consts = (const struct wined3d_ffp_vs_constants *)(data + vs_extra->offset);

    wgpu_set_render_state(device, WINED3D_RS_ALPHATESTENABLE, state->extra_ps_args.alpha_func != WINED3D_CMP_ALWAYS);
    wgpu_set_render_state(device, WINED3D_RS_ALPHAFUNC, state->extra_ps_args.alpha_func);
    if (ps_consts)
        wgpu_set_render_state(device, WINED3D_RS_ALPHAREF, lrintf(ps_consts->alpha_test_ref * 255.0f));
    wgpu_set_render_state(device, WINED3D_RS_SRGBWRITEENABLE, state->extra_ps_args.srgb_write);
    wgpu_set_render_state(device, WINED3D_RS_CLIPPLANEENABLE, state->extra_vs_args.clip_planes);
    if (vs_consts && state->extra_vs_args.clip_planes)
    {
        for (i = 0; i < WINED3D_MAX_CLIP_DISTANCES; ++i)
        {
            struct d3dgpu_cmd_set_clip_plane *cmd;

            if (!(state->extra_vs_args.clip_planes & (1u << i)))
                continue;
            if ((cmd = wgpu_cmd(device, D3DGPU_OP_SET_CLIP_PLANE, sizeof(*cmd))))
            {
                cmd->index = i;
                memcpy(cmd->plane, &vs_consts->clip_planes[i], sizeof(cmd->plane));
            }
        }
    }
    wgpu_set_render_state(device, WINED3D_RS_SHADEMODE, state->extra_ps_args.flat_shading ? 1 : 2);
}

static void wgpu_apply_sampler(struct wined3d_device_wgpu *device, unsigned int index,
        const struct wined3d_sampler *sampler)
{
    static const struct wined3d_sampler_desc defaults =
    {
        WINED3D_TADDRESS_WRAP, WINED3D_TADDRESS_WRAP, WINED3D_TADDRESS_WRAP, {0.0f, 0.0f, 0.0f, 0.0f},
        WINED3D_TEXF_POINT, WINED3D_TEXF_POINT, WINED3D_TEXF_NONE, 0.0f, 0.0f, 1000.0f, 0, 1,
    };
    const struct wined3d_sampler_desc *desc = sampler ? &sampler->desc : &defaults;
    struct wined3d_color border;
    static const uint32_t states[] =
    {
        WINED3D_SAMP_ADDRESS_U, WINED3D_SAMP_ADDRESS_V, WINED3D_SAMP_ADDRESS_W, WINED3D_SAMP_BORDER_COLOR,
        WINED3D_SAMP_MAG_FILTER, WINED3D_SAMP_MIN_FILTER, WINED3D_SAMP_MIP_FILTER, WINED3D_SAMP_MIPMAP_LOD_BIAS,
        WINED3D_SAMP_MAX_MIP_LEVEL, WINED3D_SAMP_MAX_ANISOTROPY, WINED3D_SAMP_SRGB_TEXTURE,
    };
    uint32_t values[ARRAY_SIZE(states)];
    struct d3dgpu_cmd_set_sampler_state *cmd;
    unsigned int i;

    border.r = desc->border_color[0];
    border.g = desc->border_color[1];
    border.b = desc->border_color[2];
    border.a = desc->border_color[3];
    values[0] = desc->address_u;
    values[1] = desc->address_v;
    values[2] = desc->address_w;
    values[3] = d3dcolor_from_wined3d(&border);
    values[4] = desc->max_anisotropy > 1 && desc->mag_filter != WINED3D_TEXF_POINT
            ? WINED3D_TEXF_ANISOTROPIC : desc->mag_filter;
    values[5] = desc->max_anisotropy > 1 && desc->min_filter != WINED3D_TEXF_POINT
            ? WINED3D_TEXF_ANISOTROPIC : desc->min_filter;
    values[6] = desc->mip_filter;
    values[7] = float_bits(desc->lod_bias);
    values[8] = desc->mip_base_level;
    values[9] = desc->max_anisotropy;
    values[10] = desc->srgb_decode;
    for (i = 0; i < ARRAY_SIZE(states); ++i)
    {
        if ((cmd = wgpu_cmd(device, D3DGPU_OP_SET_SAMPLER_STATE, sizeof(*cmd))))
        {
            cmd->sampler = index;
            cmd->state = states[i];
            cmd->value = values[i];
        }
    }
}

static void wgpu_bind_texture(struct wined3d_device_wgpu *device, struct wined3d_context *context,
        unsigned int index, struct wined3d_shader_resource_view *view)
{
    struct d3dgpu_cmd_set_texture *cmd;
    struct wined3d_texture *texture;
    uint32_t id = 0;

    if (view && view->resource->type != WINED3D_RTYPE_BUFFER)
    {
        texture = texture_from_resource(view->resource);
        wined3d_texture_load(texture, context, FALSE);
        id = wined3d_texture_wgpu(texture)->id;
    }
    if ((cmd = wgpu_cmd(device, D3DGPU_OP_SET_TEXTURE, sizeof(*cmd))))
    {
        cmd->sampler = index;
        cmd->texture = id;
    }
}

static void wgpu_bind_target(struct wined3d_device_wgpu *device, struct wined3d_context *context,
        int index, struct wined3d_rendertarget_view *view)
{
    struct wined3d_texture *texture = NULL;
    unsigned int sub_resource_idx = 0;
    struct d3dgpu_cmd_set_render_target *rt;
    struct d3dgpu_cmd_set_depth_stencil *ds;

    if (view && view->format->id != WINED3DFMT_NULL && view->resource->type != WINED3D_RTYPE_BUFFER)
    {
        texture = texture_from_resource(view->resource);
        sub_resource_idx = view->sub_resource_idx;
        wined3d_rendertarget_view_load_location(view, context, WINED3D_LOCATION_TEXTURE_RGB);
    }
    if (index < 0)
    {
        if ((ds = wgpu_cmd(device, D3DGPU_OP_SET_DEPTH_STENCIL, sizeof(*ds))) && texture)
        {
            ds->texture = wined3d_texture_wgpu(texture)->id;
            ds->face = sub_resource_idx / texture->level_count;
            ds->level = sub_resource_idx % texture->level_count;
        }
    }
    else if ((rt = wgpu_cmd(device, D3DGPU_OP_SET_RENDER_TARGET, sizeof(*rt))))
    {
        rt->index = index;
        if (texture)
        {
            rt->texture = wined3d_texture_wgpu(texture)->id;
            rt->face = sub_resource_idx / texture->level_count;
            rt->level = sub_resource_idx % texture->level_count;
        }
    }
}

static unsigned int wgpu_prim_count(enum wined3d_primitive_type type, unsigned int count)
{
    switch (type)
    {
        case WINED3D_PT_POINTLIST: return count;
        case WINED3D_PT_LINELIST: return count / 2;
        case WINED3D_PT_LINESTRIP: return count > 1 ? count - 1 : 0;
        case WINED3D_PT_TRIANGLELIST: return count / 3;
        case WINED3D_PT_TRIANGLESTRIP:
        case WINED3D_PT_TRIANGLEFAN: return count > 2 ? count - 2 : 0;
        default:
            FIXME("Unhandled primitive type %s.\n", debug_d3dprimitivetype(type));
            return 0;
    }
}

static void adapter_wgpu_draw_primitive(struct wined3d_device *device,
        const struct wined3d_state *state, const struct wined3d_draw_parameters *parameters)
{
    struct wined3d_device_wgpu *device_wgpu = wined3d_device_wgpu(device);
    const struct wined3d_direct_draw_parameters *direct = &parameters->u.direct;
    struct wined3d_context *context;
    struct d3dgpu_cmd_set_object *obj;
    unsigned int i, prim_count;
    uint32_t vs, ps, decl;

    TRACE("device %p, state %p, parameters %p.\n", device, state, parameters);

    if (parameters->indirect)
    {
        FIXME("Indirect draws are not supported.\n");
        return;
    }
    if (!state->vertex_declaration)
    {
        WARN("Draw without a vertex declaration.\n");
        return;
    }
    if (!(prim_count = wgpu_prim_count(state->primitive_type, direct->index_count)))
        return;

    context = context_acquire(device, NULL, 0);

    /* Targets, then textures and buffers (loading them may emit uploads). */
    for (i = 0; i < 4; ++i)
        wgpu_bind_target(device_wgpu, context, i, i < ARRAY_SIZE(state->fb.render_targets) ? state->fb.render_targets[i] : NULL);
    wgpu_bind_target(device_wgpu, context, -1, state->fb.depth_stencil);
    for (i = 0; i < 16; ++i)
    {
        wgpu_bind_texture(device_wgpu, context, i, state->shader_resource_view[WINED3D_SHADER_TYPE_PIXEL][i]);
        wgpu_apply_sampler(device_wgpu, i, state->sampler[WINED3D_SHADER_TYPE_PIXEL][i]);
    }
    for (i = 0; i < 4; ++i)
    {
        wgpu_bind_texture(device_wgpu, context, D3DGPU_VERTEX_SAMPLER_BASE + i,
                state->shader_resource_view[WINED3D_SHADER_TYPE_VERTEX][i]);
        wgpu_apply_sampler(device_wgpu, D3DGPU_VERTEX_SAMPLER_BASE + i, state->sampler[WINED3D_SHADER_TYPE_VERTEX][i]);
    }

    for (i = 0; i < WINED3D_MAX_STREAMS; ++i)
    {
        const struct wined3d_stream_state *stream = &state->streams[i];
        struct d3dgpu_cmd_set_stream_source *src;
        struct d3dgpu_cmd_set_stream_freq *freq;

        if (stream->buffer)
            wined3d_buffer_load(stream->buffer, context, state);
        if ((src = wgpu_cmd(device_wgpu, D3DGPU_OP_SET_STREAM_SOURCE, sizeof(*src))))
        {
            src->stream = i;
            src->buffer = stream->buffer && stream->buffer->buffer_object
                    ? wined3d_bo_wgpu(stream->buffer->buffer_object)->id : 0;
            src->offset = stream->offset;
            src->stride = stream->stride;
        }
        if ((freq = wgpu_cmd(device_wgpu, D3DGPU_OP_SET_STREAM_FREQ, sizeof(*freq))))
        {
            freq->stream = i;
            freq->value = (stream->flags & WINED3DSTREAMSOURCE_INDEXEDDATA ? 0x40000000 : 0)
                    | (stream->flags & WINED3DSTREAMSOURCE_INSTANCEDATA ? 0x80000000 : 0)
                    | (stream->frequency ? stream->frequency : 1);
        }
    }
    if (parameters->indexed && state->index_buffer)
    {
        struct d3dgpu_cmd_set_indices *indices;

        wined3d_buffer_load(state->index_buffer, context, state);
        if ((indices = wgpu_cmd(device_wgpu, D3DGPU_OP_SET_INDICES, sizeof(*indices))))
        {
            indices->buffer = state->index_buffer->buffer_object
                    ? wined3d_bo_wgpu(state->index_buffer->buffer_object)->id : 0;
            indices->format = state->index_format == WINED3DFMT_R32_UINT ? 102 : 101; /* D3DFMT_INDEX32/16 */
        }
    }

    vs = wgpu_shader_id(device_wgpu, state->shader[WINED3D_SHADER_TYPE_VERTEX]);
    ps = wgpu_shader_id(device_wgpu, state->shader[WINED3D_SHADER_TYPE_PIXEL]);
    decl = wgpu_vertex_decl_id(device_wgpu, state->vertex_declaration);
    if (!vs || !ps || !decl)
    {
        WARN("Skipping a draw without shaders (vs %u, ps %u) or vertex declaration (%u).\n", vs, ps, decl);
        context_release(context);
        return;
    }
    if (vs != device_wgpu->vs_id && (obj = wgpu_cmd(device_wgpu, D3DGPU_OP_SET_VERTEX_SHADER, sizeof(*obj))))
        obj->id = device_wgpu->vs_id = vs;
    if (ps != device_wgpu->ps_id && (obj = wgpu_cmd(device_wgpu, D3DGPU_OP_SET_PIXEL_SHADER, sizeof(*obj))))
        obj->id = device_wgpu->ps_id = ps;
    if (decl != device_wgpu->decl_id && (obj = wgpu_cmd(device_wgpu, D3DGPU_OP_SET_VERTEX_DECL, sizeof(*obj))))
        obj->id = device_wgpu->decl_id = decl;

    wgpu_upload_constants(device_wgpu, context, state, WINED3D_SHADER_TYPE_VERTEX);
    wgpu_upload_constants(device_wgpu, context, state, WINED3D_SHADER_TYPE_PIXEL);
    device_wgpu->consts_valid = true;

    wgpu_apply_render_states(device_wgpu, context, state);
    {
        const struct wined3d_viewport *vp = &state->viewports[0];
        struct d3dgpu_cmd_set_viewport *v;
        struct d3dgpu_cmd_set_scissor *s;

        if ((v = wgpu_cmd(device_wgpu, D3DGPU_OP_SET_VIEWPORT, sizeof(*v))))
        {
            v->x = max(0, lrintf(vp->x));
            v->y = max(0, lrintf(vp->y));
            v->width = lrintf(vp->width);
            v->height = lrintf(vp->height);
            v->min_z = vp->min_z;
            v->max_z = vp->max_z;
        }
        if ((s = wgpu_cmd(device_wgpu, D3DGPU_OP_SET_SCISSOR, sizeof(*s))))
        {
            s->rect.x1 = state->scissor_rects[0].left;
            s->rect.y1 = state->scissor_rects[0].top;
            s->rect.x2 = state->scissor_rects[0].right;
            s->rect.y2 = state->scissor_rects[0].bottom;
        }
    }

    if (parameters->indexed)
    {
        struct d3dgpu_cmd_draw_indexed *cmd;
        unsigned int index_size = state->index_format == WINED3DFMT_R32_UINT ? 4 : 2;

        if ((cmd = wgpu_cmd(device_wgpu, D3DGPU_OP_DRAW_INDEXED, sizeof(*cmd))))
        {
            cmd->prim = state->primitive_type;
            cmd->base_vertex = direct->base_vertex_idx;
            cmd->min_index = 0;
            cmd->num_vertices = ~0u;
            cmd->start_index = direct->start_idx + state->index_offset / index_size;
            cmd->prim_count = prim_count;
        }
    }
    else
    {
        struct d3dgpu_cmd_draw *cmd;

        if ((cmd = wgpu_cmd(device_wgpu, D3DGPU_OP_DRAW, sizeof(*cmd))))
        {
            cmd->prim = state->primitive_type;
            cmd->start_vertex = direct->start_idx;
            cmd->prim_count = prim_count;
        }
    }

    /* The draw wrote the targets on the GPU. */
    for (i = 0; i < ARRAY_SIZE(state->fb.render_targets); ++i)
    {
        struct wined3d_rendertarget_view *rtv = state->fb.render_targets[i];

        if (!rtv || rtv->format->id == WINED3DFMT_NULL || rtv->resource->type == WINED3D_RTYPE_BUFFER)
            continue;
        if (!wined3d_blend_state_get_writemask(state->blend_state, i))
            continue;
        wined3d_rendertarget_view_validate_location(rtv, WINED3D_LOCATION_TEXTURE_RGB);
        wined3d_rendertarget_view_invalidate_location(rtv, ~WINED3D_LOCATION_TEXTURE_RGB);
    }
    if (state->fb.depth_stencil && (!state->depth_stencil_state || state->depth_stencil_state->writes_ds))
    {
        wined3d_rendertarget_view_validate_location(state->fb.depth_stencil, WINED3D_LOCATION_TEXTURE_RGB);
        wined3d_rendertarget_view_invalidate_location(state->fb.depth_stencil, ~WINED3D_LOCATION_TEXTURE_RGB);
    }

    context_release(context);
}

/* ---- Blitter ----------------------------------------------------------- */

struct wgpu_blitter
{
    struct wined3d_blitter blitter;
};

static void wgpu_blitter_destroy(struct wined3d_blitter *blitter, struct wined3d_context *context)
{
    struct wined3d_blitter *next;

    if ((next = blitter->next))
        next->ops->blitter_destroy(next, context);
    free(blitter);
}

static void wgpu_blitter_clear(struct wined3d_blitter *blitter, struct wined3d_device *device,
        unsigned int rt_count, const struct wined3d_fb_state *fb, unsigned int rect_count, const RECT *clear_rects,
        const RECT *draw_rect, uint32_t flags, const struct wined3d_color *colour, float depth, unsigned int stencil)
{
    struct wined3d_device_wgpu *device_wgpu = wined3d_device_wgpu(device);
    struct wined3d_rendertarget_view *view;
    struct wined3d_context *context;
    struct d3dgpu_cmd_clear *cmd;
    unsigned int i, n = 0;
    RECT r;

    TRACE("blitter %p, device %p, rt_count %u, fb %p, rect_count %u, clear_rects %p, "
            "draw_rect %s, flags %#x, colour %s, depth %.8e, stencil %#x.\n",
            blitter, device, rt_count, fb, rect_count, clear_rects,
            wine_dbgstr_rect(draw_rect), flags, debug_color(colour), depth, stencil);

    context = context_acquire(device, NULL, 0);
    for (i = 0; i < 4; ++i)
        wgpu_bind_target(device_wgpu, context, i,
                (flags & WINED3DCLEAR_TARGET) && i < rt_count ? fb->render_targets[i] : NULL);
    wgpu_bind_target(device_wgpu, context, -1,
            (flags & (WINED3DCLEAR_ZBUFFER | WINED3DCLEAR_STENCIL)) ? fb->depth_stencil : NULL);
    device_wgpu->decl_id = ~0u;
    {
        struct wined3d_rendertarget_view *v = rt_count && fb->render_targets[0] ? fb->render_targets[0] : fb->depth_stencil;

        if (v)
            wgpu_full_viewport(device_wgpu, v->width, v->height);
    }

    if (!(cmd = wgpu_cmd(device_wgpu, D3DGPU_OP_CLEAR, sizeof(*cmd) + (rect_count ? rect_count : 1) * 16)))
    {
        context_release(context);
        return;
    }
    cmd->flags = flags & (WINED3DCLEAR_TARGET | WINED3DCLEAR_ZBUFFER | WINED3DCLEAR_STENCIL);
    cmd->color = d3dcolor_from_wined3d(colour);
    cmd->z = depth;
    cmd->stencil = stencil;
    for (i = 0; i < max(rect_count, 1); ++i)
    {
        struct d3dgpu_rect *dst = (struct d3dgpu_rect *)(cmd + 1) + n;

        if (rect_count)
            IntersectRect(&r, &clear_rects[i], draw_rect);
        else
            r = *draw_rect;
        if (IsRectEmpty(&r))
            continue;
        dst->x1 = r.left;
        dst->y1 = r.top;
        dst->x2 = r.right;
        dst->y2 = r.bottom;
        ++n;
    }
    cmd->rect_count = n;
    cmd->h.size = sizeof(*cmd) + n * 16;
    device_wgpu->cmd_size -= ((rect_count ? rect_count : 1) - n) * 16;

    if (flags & WINED3DCLEAR_TARGET)
    {
        for (i = 0; i < rt_count; ++i)
        {
            if (!(view = fb->render_targets[i]) || view->resource->type == WINED3D_RTYPE_BUFFER)
                continue;
            wined3d_rendertarget_view_validate_location(view, WINED3D_LOCATION_TEXTURE_RGB);
            wined3d_rendertarget_view_invalidate_location(view, ~WINED3D_LOCATION_TEXTURE_RGB);
        }
    }
    if ((flags & (WINED3DCLEAR_ZBUFFER | WINED3DCLEAR_STENCIL)) && (view = fb->depth_stencil))
    {
        wined3d_rendertarget_view_validate_location(view, WINED3D_LOCATION_TEXTURE_RGB);
        wined3d_rendertarget_view_invalidate_location(view, ~WINED3D_LOCATION_TEXTURE_RGB);
    }
    context_release(context);
}

static DWORD wgpu_blitter_blit(struct wined3d_blitter *blitter, enum wined3d_blit_op op,
        struct wined3d_context *context, struct wined3d_texture *src_texture, unsigned int src_sub_resource_idx,
        DWORD src_location, const RECT *src_rect, struct wined3d_texture *dst_texture,
        unsigned int dst_sub_resource_idx, DWORD dst_location, const RECT *dst_rect,
        const struct wined3d_color_key *colour_key, enum wined3d_texture_filter_type filter,
        const struct wined3d_format *resolve_format)
{
    struct wined3d_blitter *next;

    /* Blits run on the CPU for now: the next blitter reads both
     * sub-resources back to system memory. */
    if (!(next = blitter->next))
    {
        ERR("No blitter to handle blit op %#x.\n", op);
        return dst_location;
    }
    return next->ops->blitter_blit(next, op, context, src_texture, src_sub_resource_idx, src_location,
            src_rect, dst_texture, dst_sub_resource_idx, dst_location, dst_rect, colour_key, filter, resolve_format);
}

static const struct wined3d_blitter_ops wgpu_blitter_ops =
{
    wgpu_blitter_destroy,
    wgpu_blitter_clear,
    wgpu_blitter_blit,
};

/* ---- Swapchain --------------------------------------------------------- */

static void wgpu_present_to_window(struct wined3d_swapchain *swapchain, struct wined3d_texture *texture,
        const RECT *src_rect, const RECT *dst_rect)
{
    struct wined3d_context *context;
    struct wined3d_bo_address data;
    unsigned int row_pitch, slice_pitch;
    BITMAPINFO info = {{sizeof(info.bmiHeader)}};
    RECT src, dst;

    if (texture->resource.format->byte_count != 4)
    {
        FIXME("Presenting %s back buffers is not supported.\n", debug_d3dformat(texture->resource.format->id));
        return;
    }
    context = context_acquire(swapchain->device, NULL, 0);
    if (!wined3d_texture_load_location(texture, 0, context, WINED3D_LOCATION_SYSMEM))
    {
        context_release(context);
        return;
    }
    context_release(context);
    wined3d_texture_get_bo_address(texture, 0, &data, WINED3D_LOCATION_SYSMEM);
    wined3d_texture_get_pitch(texture, 0, &row_pitch, &slice_pitch);

    src = *src_rect;
    dst = *dst_rect;
    info.bmiHeader.biWidth = row_pitch / 4;
    info.bmiHeader.biHeight = -(LONG)texture->resource.height;
    info.bmiHeader.biPlanes = 1;
    info.bmiHeader.biBitCount = 32;
    info.bmiHeader.biCompression = BI_RGB;
    StretchDIBits(swapchain->dc, dst.left, dst.top, dst.right - dst.left, dst.bottom - dst.top,
            src.left, src.top, src.right - src.left, src.bottom - src.top, data.addr, &info,
            DIB_RGB_COLORS, SRCCOPY);
}

static void swapchain_wgpu_present(struct wined3d_swapchain *swapchain,
        const RECT *src_rect, const RECT *dst_rect, unsigned int swap_interval, uint32_t flags)
{
    struct wined3d_texture *back = swapchain->back_buffers[0];

    TRACE("swapchain %p, src_rect %s, dst_rect %s, swap_interval %u, flags %#x.\n", swapchain,
            wine_dbgstr_rect(src_rect), wine_dbgstr_rect(dst_rect), swap_interval, flags);

    wgpu_present_to_window(swapchain, back, src_rect, dst_rect);
    if (swapchain->state.desc.swap_effect == WINED3D_SWAP_EFFECT_DISCARD)
        wined3d_texture_validate_location(back, 0, WINED3D_LOCATION_DISCARDED);
}

static void swapchain_wgpu_frontbuffer_updated(struct wined3d_swapchain *swapchain)
{
    TRACE("swapchain %p.\n", swapchain);
}

static const struct wined3d_swapchain_ops swapchain_wgpu_ops =
{
    swapchain_wgpu_present,
    swapchain_wgpu_frontbuffer_updated,
};

/* ---- Adapter ----------------------------------------------------------- */

static const struct wined3d_state_entry_template misc_state_template_wgpu[] =
{
    {STATE_CONSTANT_BUFFER(WINED3D_SHADER_TYPE_VERTEX),   {STATE_CONSTANT_BUFFER(WINED3D_SHADER_TYPE_VERTEX),   state_nop}},
    {STATE_CONSTANT_BUFFER(WINED3D_SHADER_TYPE_HULL),     {STATE_CONSTANT_BUFFER(WINED3D_SHADER_TYPE_HULL),     state_nop}},
    {STATE_CONSTANT_BUFFER(WINED3D_SHADER_TYPE_DOMAIN),   {STATE_CONSTANT_BUFFER(WINED3D_SHADER_TYPE_DOMAIN),   state_nop}},
    {STATE_CONSTANT_BUFFER(WINED3D_SHADER_TYPE_GEOMETRY), {STATE_CONSTANT_BUFFER(WINED3D_SHADER_TYPE_GEOMETRY), state_nop}},
    {STATE_CONSTANT_BUFFER(WINED3D_SHADER_TYPE_PIXEL),    {STATE_CONSTANT_BUFFER(WINED3D_SHADER_TYPE_PIXEL),    state_nop}},
    {STATE_CONSTANT_BUFFER(WINED3D_SHADER_TYPE_COMPUTE),  {STATE_CONSTANT_BUFFER(WINED3D_SHADER_TYPE_COMPUTE),  state_nop}},
    {STATE_GRAPHICS_SHADER_RESOURCE_BINDING,              {STATE_GRAPHICS_SHADER_RESOURCE_BINDING,              state_nop}},
    {STATE_GRAPHICS_UNORDERED_ACCESS_VIEW_BINDING,        {STATE_GRAPHICS_UNORDERED_ACCESS_VIEW_BINDING,        state_nop}},
    {STATE_COMPUTE_SHADER_RESOURCE_BINDING,               {STATE_COMPUTE_SHADER_RESOURCE_BINDING,               state_nop}},
    {STATE_COMPUTE_UNORDERED_ACCESS_VIEW_BINDING,         {STATE_COMPUTE_UNORDERED_ACCESS_VIEW_BINDING,         state_nop}},
    {STATE_STREAM_OUTPUT,                                 {STATE_STREAM_OUTPUT,                                 state_nop}},
    {STATE_BLEND,                                         {STATE_BLEND,                                         state_nop}},
    {STATE_BLEND_FACTOR,                                  {STATE_BLEND_FACTOR,                                  state_nop}},
    {STATE_SAMPLE_MASK,                                   {STATE_SAMPLE_MASK,                                   state_nop}},
    {STATE_STREAMSRC,                                     {STATE_STREAMSRC,                                     state_nop}},
    {STATE_VDECL,                                         {STATE_VDECL,                                         state_nop}},
    {STATE_DEPTH_STENCIL,                                 {STATE_DEPTH_STENCIL,                                 state_nop}},
    {STATE_STENCIL_REF,                                   {STATE_STENCIL_REF,                                   state_nop}},
    {STATE_DEPTH_BOUNDS,                                  {STATE_DEPTH_BOUNDS,                                  state_nop}},
    {STATE_RASTERIZER,                                    {STATE_RASTERIZER,                                    state_nop}},
    {STATE_SCISSORRECT,                                   {STATE_SCISSORRECT,                                   state_nop}},
    {STATE_VIEWPORT,                                      {STATE_VIEWPORT,                                      state_nop}},
    {STATE_INDEXBUFFER,                                   {STATE_INDEXBUFFER,                                   state_nop}},
    {STATE_RENDER(WINED3D_RS_LINEPATTERN),                {STATE_RENDER(WINED3D_RS_LINEPATTERN),                state_nop}},
    {STATE_RENDER(WINED3D_RS_DITHERENABLE),               {STATE_RENDER(WINED3D_RS_DITHERENABLE),               state_nop}},
    {STATE_RENDER(WINED3D_RS_MULTISAMPLEANTIALIAS),       {STATE_RENDER(WINED3D_RS_MULTISAMPLEANTIALIAS),       state_nop}},
    /* Fixed-function states that arrive as shaders and constants. */
    {STATE_RENDER(WINED3D_RS_FOGENABLE),                  {STATE_RENDER(WINED3D_RS_FOGENABLE),                  state_nop}},
    {STATE_RENDER(WINED3D_RS_SPECULARENABLE),             {STATE_RENDER(WINED3D_RS_SPECULARENABLE),             state_nop}},
    {STATE_RENDER(WINED3D_RS_COLORKEYENABLE),             {STATE_RENDER(WINED3D_RS_COLORKEYENABLE),             state_nop}},
    {STATE_RENDER(WINED3D_RS_RANGEFOGENABLE),             {STATE_RENDER(WINED3D_RS_RANGEFOGENABLE),             state_nop}},
    {STATE_RENDER(WINED3D_RS_LIGHTING),                   {STATE_RENDER(WINED3D_RS_LIGHTING),                   state_nop}},
    {STATE_RENDER(WINED3D_RS_COLORVERTEX),                {STATE_RENDER(WINED3D_RS_COLORVERTEX),                state_nop}},
    {STATE_RENDER(WINED3D_RS_LOCALVIEWER),                {STATE_RENDER(WINED3D_RS_LOCALVIEWER),                state_nop}},
    {STATE_RENDER(WINED3D_RS_NORMALIZENORMALS),           {STATE_RENDER(WINED3D_RS_NORMALIZENORMALS),           state_nop}},
    {STATE_RENDER(WINED3D_RS_DIFFUSEMATERIALSOURCE),      {STATE_RENDER(WINED3D_RS_DIFFUSEMATERIALSOURCE),      state_nop}},
    {STATE_RENDER(WINED3D_RS_SPECULARMATERIALSOURCE),     {STATE_RENDER(WINED3D_RS_SPECULARMATERIALSOURCE),     state_nop}},
    {STATE_RENDER(WINED3D_RS_AMBIENTMATERIALSOURCE),      {STATE_RENDER(WINED3D_RS_AMBIENTMATERIALSOURCE),      state_nop}},
    {STATE_RENDER(WINED3D_RS_EMISSIVEMATERIALSOURCE),     {STATE_RENDER(WINED3D_RS_EMISSIVEMATERIALSOURCE),     state_nop}},
    {STATE_RENDER(WINED3D_RS_VERTEXBLEND),                {STATE_RENDER(WINED3D_RS_VERTEXBLEND),                state_nop}},
    {STATE_BASEVERTEXINDEX,                               {STATE_STREAMSRC}},
    {STATE_FRAMEBUFFER,                                   {STATE_FRAMEBUFFER,                                   state_nop}},
    {STATE_SHADER(WINED3D_SHADER_TYPE_PIXEL),             {STATE_SHADER(WINED3D_SHADER_TYPE_PIXEL),             state_nop}},
    {STATE_SHADER(WINED3D_SHADER_TYPE_HULL),              {STATE_SHADER(WINED3D_SHADER_TYPE_HULL),              state_nop}},
    {STATE_SHADER(WINED3D_SHADER_TYPE_DOMAIN),            {STATE_SHADER(WINED3D_SHADER_TYPE_DOMAIN),            state_nop}},
    {STATE_SHADER(WINED3D_SHADER_TYPE_GEOMETRY),          {STATE_SHADER(WINED3D_SHADER_TYPE_GEOMETRY),          state_nop}},
    {STATE_SHADER(WINED3D_SHADER_TYPE_COMPUTE),           {STATE_SHADER(WINED3D_SHADER_TYPE_COMPUTE),           state_nop}},
    {0},
};

static void adapter_wgpu_destroy(struct wined3d_adapter *adapter)
{
    wined3d_adapter_cleanup(adapter);
    free(adapter);
}

static HRESULT adapter_wgpu_create_device(struct wined3d *wined3d, const struct wined3d_adapter *adapter,
        enum wined3d_device_type device_type, HWND focus_window, unsigned int flags, BYTE surface_alignment,
        const enum wined3d_feature_level *levels, unsigned int level_count,
        struct wined3d_device_parent *device_parent, struct wined3d_device **device)
{
    static const BOOL supported_extensions[] = {TRUE};
    struct wined3d_device_wgpu *device_wgpu;
    HRESULT hr;

    if (!(device_wgpu = calloc(1, sizeof(*device_wgpu))))
        return E_OUTOFMEMORY;
    device_wgpu->cmd_capacity = 1 << 20;
    if (!(device_wgpu->cmds = malloc(device_wgpu->cmd_capacity)))
    {
        free(device_wgpu);
        return E_OUTOFMEMORY;
    }
    device_wgpu->cmd_size = sizeof(struct d3dgpu_batch_header);
    device_wgpu->decl_id = ~0u;

    if (FAILED(hr = wined3d_device_init(&device_wgpu->d, wined3d, adapter->ordinal, device_type, focus_window,
            flags, surface_alignment, levels, level_count, supported_extensions, device_parent)))
    {
        WARN("Failed to initialize device, hr %#lx.\n", hr);
        free(device_wgpu->cmds);
        free(device_wgpu);
        return hr;
    }

    *device = &device_wgpu->d;
    return WINED3D_OK;
}

static void adapter_wgpu_destroy_device(struct wined3d_device *device)
{
    struct wined3d_device_wgpu *device_wgpu = wined3d_device_wgpu(device);

    wined3d_device_cleanup(device);
    wgpu_flush(device_wgpu);
    free(device_wgpu->decls);
    free(device_wgpu->cmds);
    free(device_wgpu);
}

static struct wined3d_context *adapter_wgpu_acquire_context(struct wined3d_device *device,
        struct wined3d_texture *texture, unsigned int sub_resource_idx)
{
    wined3d_from_cs(device->cs);

    if (!device->context_count)
        return NULL;
    return &wined3d_device_wgpu(device)->context;
}

static void adapter_wgpu_release_context(struct wined3d_context *context)
{
}

static void adapter_wgpu_get_wined3d_caps(const struct wined3d_adapter *adapter, struct wined3d_caps *caps)
{
    caps->ddraw_caps.dds_caps |= WINEDDSCAPS_BACKBUFFER
            | WINEDDSCAPS_COMPLEX
            | WINEDDSCAPS_FRONTBUFFER
            | WINEDDSCAPS_3DDEVICE
            | WINEDDSCAPS_VIDEOMEMORY
            | WINEDDSCAPS_OWNDC
            | WINEDDSCAPS_LOCALVIDMEM
            | WINEDDSCAPS_NONLOCALVIDMEM;
    caps->ddraw_caps.caps |= WINEDDCAPS_3D;

    caps->PrimitiveMiscCaps |= WINED3DPMISCCAPS_BLENDOP
            | WINED3DPMISCCAPS_INDEPENDENTWRITEMASKS
            | WINED3DPMISCCAPS_MRTINDEPENDENTBITDEPTHS
            | WINED3DPMISCCAPS_POSTBLENDSRGBCONVERT
            | WINED3DPMISCCAPS_SEPARATEALPHABLEND;

    caps->RasterCaps |= WINED3DPRASTERCAPS_MIPMAPLODBIAS | WINED3DPRASTERCAPS_ANISOTROPY;
    caps->TextureFilterCaps |= WINED3DPTFILTERCAPS_MAGFANISOTROPIC | WINED3DPTFILTERCAPS_MINFANISOTROPIC;
    caps->MaxAnisotropy = 16;

    caps->SrcBlendCaps |= WINED3DPBLENDCAPS_BLENDFACTOR;
    caps->DestBlendCaps |= WINED3DPBLENDCAPS_BLENDFACTOR | WINED3DPBLENDCAPS_SRCALPHASAT;

    caps->TextureCaps |= WINED3DPTEXTURECAPS_VOLUMEMAP
            | WINED3DPTEXTURECAPS_MIPVOLUMEMAP
            | WINED3DPTEXTURECAPS_VOLUMEMAP_POW2
            | WINED3DPTEXTURECAPS_CUBEMAP
            | WINED3DPTEXTURECAPS_MIPCUBEMAP
            | WINED3DPTEXTURECAPS_CUBEMAP_POW2;
    caps->VolumeTextureFilterCaps |= WINED3DPTFILTERCAPS_MAGFLINEAR
            | WINED3DPTFILTERCAPS_MAGFPOINT
            | WINED3DPTFILTERCAPS_MINFLINEAR
            | WINED3DPTFILTERCAPS_MINFPOINT
            | WINED3DPTFILTERCAPS_MIPFLINEAR
            | WINED3DPTFILTERCAPS_MIPFPOINT
            | WINED3DPTFILTERCAPS_LINEAR
            | WINED3DPTFILTERCAPS_LINEARMIPLINEAR
            | WINED3DPTFILTERCAPS_LINEARMIPNEAREST
            | WINED3DPTFILTERCAPS_MIPLINEAR
            | WINED3DPTFILTERCAPS_MIPNEAREST
            | WINED3DPTFILTERCAPS_NEAREST;
    caps->CubeTextureFilterCaps |= caps->VolumeTextureFilterCaps
            | WINED3DPTFILTERCAPS_MAGFANISOTROPIC
            | WINED3DPTFILTERCAPS_MINFANISOTROPIC;
    caps->VolumeTextureAddressCaps |= WINED3DPTADDRESSCAPS_INDEPENDENTUV
            | WINED3DPTADDRESSCAPS_CLAMP
            | WINED3DPTADDRESSCAPS_WRAP
            | WINED3DPTADDRESSCAPS_BORDER
            | WINED3DPTADDRESSCAPS_MIRROR
            | WINED3DPTADDRESSCAPS_MIRRORONCE;
    caps->MaxVolumeExtent = 2048;
    caps->TextureAddressCaps |= WINED3DPTADDRESSCAPS_BORDER
            | WINED3DPTADDRESSCAPS_MIRROR
            | WINED3DPTADDRESSCAPS_MIRRORONCE;

    caps->StencilCaps |= WINED3DSTENCILCAPS_DECR
            | WINED3DSTENCILCAPS_INCR
            | WINED3DSTENCILCAPS_TWOSIDED;

    caps->DeclTypes |= WINED3DDTCAPS_FLOAT16_2 | WINED3DDTCAPS_FLOAT16_4;

    caps->MaxPixelShader30InstructionSlots = WINED3DMAX30SHADERINSTRUCTIONS;
    caps->MaxVertexShader30InstructionSlots = WINED3DMAX30SHADERINSTRUCTIONS;
    caps->PS20Caps.temp_count = WINED3DPS20_MAX_NUMTEMPS;
    caps->VS20Caps.temp_count = WINED3DVS20_MAX_NUMTEMPS;
}

static BOOL adapter_wgpu_check_format(const struct wined3d_adapter *adapter,
        const struct wined3d_format *adapter_format, const struct wined3d_format *rt_format,
        const struct wined3d_format *ds_format)
{
    return TRUE;
}

static HRESULT adapter_wgpu_init_3d(struct wined3d_device *device)
{
    struct wined3d_device_wgpu *device_wgpu = wined3d_device_wgpu(device);
    struct wgpu_blitter *blitter;

    TRACE("device %p.\n", device);

    wined3d_context_init(&device_wgpu->context, device->swapchains[0]);
    if (!device_context_add(device, &device_wgpu->context))
    {
        ERR("Failed to add the context.\n");
        wined3d_context_cleanup(&device_wgpu->context);
        return E_FAIL;
    }

    if (!(device->blitter = wined3d_cpu_blitter_create()))
    {
        device_context_remove(device, &device_wgpu->context);
        wined3d_context_cleanup(&device_wgpu->context);
        return E_FAIL;
    }
    if ((blitter = calloc(1, sizeof(*blitter))))
    {
        blitter->blitter.ops = &wgpu_blitter_ops;
        blitter->blitter.next = device->blitter;
        device->blitter = &blitter->blitter;
    }

    wined3d_device_create_default_samplers(device, &device_wgpu->context);
    return WINED3D_OK;
}

static void adapter_wgpu_uninit_3d(struct wined3d_device *device)
{
    struct wined3d_device_wgpu *device_wgpu = wined3d_device_wgpu(device);

    TRACE("device %p.\n", device);

    wined3d_device_destroy_default_samplers(device);
    device->blitter->ops->blitter_destroy(device->blitter, NULL);
    wined3d_cs_finish(device->cs, WINED3D_CS_QUEUE_DEFAULT);
    wgpu_flush(device_wgpu);

    device_context_remove(device, &device_wgpu->context);
    wined3d_context_cleanup(&device_wgpu->context);
}

static HRESULT adapter_wgpu_create_swapchain(struct wined3d_device *device,
        const struct wined3d_swapchain_desc *desc, struct wined3d_swapchain_state_parent *state_parent,
        void *parent, const struct wined3d_parent_ops *parent_ops, struct wined3d_swapchain **swapchain)
{
    struct wined3d_swapchain *swapchain_wgpu;
    HRESULT hr;

    if (!(swapchain_wgpu = calloc(1, sizeof(*swapchain_wgpu))))
        return E_OUTOFMEMORY;
    if (FAILED(hr = wined3d_swapchain_wgpu_init(swapchain_wgpu, device, desc, state_parent, parent,
            parent_ops, &swapchain_wgpu_ops)))
    {
        WARN("Failed to initialise swapchain, hr %#lx.\n", hr);
        free(swapchain_wgpu);
        return hr;
    }
    *swapchain = swapchain_wgpu;
    return WINED3D_OK;
}

static void adapter_wgpu_destroy_swapchain(struct wined3d_swapchain *swapchain)
{
    wined3d_swapchain_cleanup(swapchain);
    free(swapchain);
}

static HRESULT adapter_wgpu_create_buffer(struct wined3d_device *device,
        const struct wined3d_buffer_desc *desc, const struct wined3d_sub_resource_data *data,
        void *parent, const struct wined3d_parent_ops *parent_ops, struct wined3d_buffer **buffer)
{
    struct wined3d_buffer *buffer_wgpu;
    HRESULT hr;

    if (!(buffer_wgpu = calloc(1, sizeof(*buffer_wgpu))))
        return E_OUTOFMEMORY;
    if (FAILED(hr = wined3d_buffer_wgpu_init(buffer_wgpu, device, desc, data, parent, parent_ops)))
    {
        WARN("Failed to initialise buffer, hr %#lx.\n", hr);
        free(buffer_wgpu);
        return hr;
    }
    *buffer = buffer_wgpu;
    return hr;
}

static void adapter_wgpu_destroy_buffer(struct wined3d_buffer *buffer)
{
    struct wined3d_device *device = buffer->resource.device;
    unsigned int swapchain_count = device->swapchain_count;

    if (swapchain_count)
        wined3d_device_incref(device);
    wined3d_buffer_cleanup(buffer);
    wined3d_cs_destroy_object(device->cs, free, buffer);
    if (swapchain_count)
        wined3d_device_decref(device);
}

static HRESULT adapter_wgpu_create_texture(struct wined3d_device *device,
        const struct wined3d_resource_desc *desc, unsigned int layer_count, unsigned int level_count,
        uint32_t flags, void *parent, const struct wined3d_parent_ops *parent_ops, struct wined3d_texture **texture)
{
    struct wined3d_texture_wgpu *texture_wgpu;
    HRESULT hr;

    if (!(texture_wgpu = wined3d_texture_allocate_object_memory(sizeof(*texture_wgpu), level_count, layer_count)))
        return E_OUTOFMEMORY;
    if (FAILED(hr = wined3d_texture_init(&texture_wgpu->t, desc, layer_count, level_count,
            flags, device, parent, parent_ops, &texture_wgpu[1], &wined3d_texture_wgpu_ops)))
    {
        WARN("Failed to initialise texture, hr %#lx.\n", hr);
        free(texture_wgpu);
        return hr;
    }
    *texture = &texture_wgpu->t;
    return hr;
}

static void wgpu_destroy_texture_object(void *object)
{
    struct wined3d_texture_wgpu *texture_wgpu = object;

    if (texture_wgpu->id)
        wgpu_destroy_object(wined3d_device_wgpu(texture_wgpu->t.resource.device), texture_wgpu->id);
    free(texture_wgpu);
}

static void adapter_wgpu_destroy_texture(struct wined3d_texture *texture)
{
    struct wined3d_device *device = texture->resource.device;
    unsigned int swapchain_count = device->swapchain_count;

    if (swapchain_count)
        wined3d_device_incref(device);
    wined3d_texture_sub_resources_destroyed(texture);
    texture->resource.parent_ops->wined3d_object_destroyed(texture->resource.parent);
    wined3d_texture_cleanup(texture);
    wined3d_cs_destroy_object(device->cs, wgpu_destroy_texture_object, wined3d_texture_wgpu(texture));
    if (swapchain_count)
        wined3d_device_decref(device);
}

static HRESULT adapter_wgpu_create_rendertarget_view(const struct wined3d_view_desc *desc,
        struct wined3d_resource *resource, void *parent, const struct wined3d_parent_ops *parent_ops,
        struct wined3d_rendertarget_view **view)
{
    struct wined3d_rendertarget_view *view_wgpu;
    HRESULT hr;

    if (!(view_wgpu = calloc(1, sizeof(*view_wgpu))))
        return E_OUTOFMEMORY;
    if (FAILED(hr = wined3d_rendertarget_view_no3d_init(view_wgpu, desc, resource, parent, parent_ops)))
    {
        free(view_wgpu);
        return hr;
    }
    *view = view_wgpu;
    return hr;
}

static void adapter_wgpu_destroy_rendertarget_view(struct wined3d_rendertarget_view *view)
{
    struct wined3d_device *device = view->resource->device;
    unsigned int swapchain_count = device->swapchain_count;

    if (swapchain_count)
        wined3d_device_incref(device);
    wined3d_rendertarget_view_cleanup(view);
    wined3d_cs_destroy_object(device->cs, free, view);
    if (swapchain_count)
        wined3d_device_decref(device);
}

static HRESULT adapter_wgpu_create_shader_resource_view(const struct wined3d_view_desc *desc,
        struct wined3d_resource *resource, void *parent, const struct wined3d_parent_ops *parent_ops,
        struct wined3d_shader_resource_view **view)
{
    struct wined3d_shader_resource_view *view_wgpu;
    HRESULT hr;

    if (!(view_wgpu = calloc(1, sizeof(*view_wgpu))))
        return E_OUTOFMEMORY;
    if (FAILED(hr = wined3d_shader_resource_view_wgpu_init(view_wgpu, desc, resource, parent, parent_ops)))
    {
        free(view_wgpu);
        return hr;
    }
    *view = view_wgpu;
    return hr;
}

static void adapter_wgpu_destroy_shader_resource_view(struct wined3d_shader_resource_view *view)
{
    struct wined3d_device *device = view->resource->device;
    unsigned int swapchain_count = device->swapchain_count;

    if (swapchain_count)
        wined3d_device_incref(device);
    wined3d_shader_resource_view_cleanup(view);
    wined3d_cs_destroy_object(device->cs, free, view);
    if (swapchain_count)
        wined3d_device_decref(device);
}

static HRESULT adapter_wgpu_create_unordered_access_view(const struct wined3d_view_desc *desc,
        struct wined3d_resource *resource, void *parent, const struct wined3d_parent_ops *parent_ops,
        struct wined3d_unordered_access_view **view)
{
    return E_NOTIMPL;
}

static void adapter_wgpu_destroy_unordered_access_view(struct wined3d_unordered_access_view *view)
{
}

static HRESULT adapter_wgpu_create_video_decoder_output_view(const struct wined3d_view_desc *desc,
        struct wined3d_texture *texture, void *parent, const struct wined3d_parent_ops *parent_ops,
        struct wined3d_decoder_output_view **view)
{
    return E_NOTIMPL;
}

static void adapter_wgpu_destroy_video_decoder_output_view(struct wined3d_decoder_output_view *view)
{
}

static HRESULT adapter_wgpu_create_sampler(struct wined3d_device *device, const struct wined3d_sampler_desc *desc,
        void *parent, const struct wined3d_parent_ops *parent_ops, struct wined3d_sampler **sampler)
{
    struct wined3d_sampler *sampler_wgpu;

    if (!(sampler_wgpu = calloc(1, sizeof(*sampler_wgpu))))
        return E_OUTOFMEMORY;
    wined3d_sampler_wgpu_init(sampler_wgpu, device, desc, parent, parent_ops);
    *sampler = sampler_wgpu;
    return WINED3D_OK;
}

static void adapter_wgpu_destroy_sampler(struct wined3d_sampler *sampler)
{
    wined3d_cs_destroy_object(sampler->device->cs, free, sampler);
}

static HRESULT adapter_wgpu_create_query(struct wined3d_device *device, enum wined3d_query_type type,
        void *parent, const struct wined3d_parent_ops *parent_ops, struct wined3d_query **query)
{
    return WINED3DERR_NOTAVAILABLE;
}

static void adapter_wgpu_destroy_query(struct wined3d_query *query)
{
}

static void adapter_wgpu_flush_context(struct wined3d_context *context)
{
    wgpu_flush(wined3d_device_wgpu(context->device));
}

static void adapter_wgpu_dispatch_compute(struct wined3d_device *device,
        const struct wined3d_state *state, const struct wined3d_dispatch_parameters *parameters)
{
    FIXME("Compute shaders are not supported yet.\n");
}

static void adapter_wgpu_clear_uav(struct wined3d_context *context,
        struct wined3d_unordered_access_view *view, const struct wined3d_uvec4 *clear_value, bool fp)
{
    FIXME("Unordered access views are not supported yet.\n");
}

static void adapter_wgpu_generate_mipmap(struct wined3d_context *context, struct wined3d_shader_resource_view *view)
{
    FIXME("Mipmap generation is not supported yet.\n");
}

static const struct wined3d_adapter_ops wined3d_adapter_wgpu_ops =
{
    .adapter_destroy = adapter_wgpu_destroy,
    .adapter_create_device = adapter_wgpu_create_device,
    .adapter_destroy_device = adapter_wgpu_destroy_device,
    .adapter_acquire_context = adapter_wgpu_acquire_context,
    .adapter_release_context = adapter_wgpu_release_context,
    .adapter_get_wined3d_caps = adapter_wgpu_get_wined3d_caps,
    .adapter_check_format = adapter_wgpu_check_format,
    .adapter_init_3d = adapter_wgpu_init_3d,
    .adapter_uninit_3d = adapter_wgpu_uninit_3d,
    .adapter_map_bo_address = adapter_wgpu_map_bo_address,
    .adapter_unmap_bo_address = adapter_wgpu_unmap_bo_address,
    .adapter_copy_bo_address = adapter_wgpu_copy_bo_address,
    .adapter_flush_bo_address = adapter_wgpu_flush_bo_address,
    .adapter_alloc_bo = adapter_wgpu_alloc_bo,
    .adapter_destroy_bo = adapter_wgpu_destroy_bo,
    .adapter_create_swapchain = adapter_wgpu_create_swapchain,
    .adapter_destroy_swapchain = adapter_wgpu_destroy_swapchain,
    .adapter_create_buffer = adapter_wgpu_create_buffer,
    .adapter_destroy_buffer = adapter_wgpu_destroy_buffer,
    .adapter_create_texture = adapter_wgpu_create_texture,
    .adapter_destroy_texture = adapter_wgpu_destroy_texture,
    .adapter_create_rendertarget_view = adapter_wgpu_create_rendertarget_view,
    .adapter_destroy_rendertarget_view = adapter_wgpu_destroy_rendertarget_view,
    .adapter_create_shader_resource_view = adapter_wgpu_create_shader_resource_view,
    .adapter_destroy_shader_resource_view = adapter_wgpu_destroy_shader_resource_view,
    .adapter_create_unordered_access_view = adapter_wgpu_create_unordered_access_view,
    .adapter_destroy_unordered_access_view = adapter_wgpu_destroy_unordered_access_view,
    .adapter_create_video_decoder_output_view = adapter_wgpu_create_video_decoder_output_view,
    .adapter_destroy_video_decoder_output_view = adapter_wgpu_destroy_video_decoder_output_view,
    .adapter_create_sampler = adapter_wgpu_create_sampler,
    .adapter_destroy_sampler = adapter_wgpu_destroy_sampler,
    .adapter_create_query = adapter_wgpu_create_query,
    .adapter_destroy_query = adapter_wgpu_destroy_query,
    .adapter_flush_context = adapter_wgpu_flush_context,
    .adapter_draw_primitive = adapter_wgpu_draw_primitive,
    .adapter_dispatch_compute = adapter_wgpu_dispatch_compute,
    .adapter_clear_uav = adapter_wgpu_clear_uav,
    .adapter_generate_mipmap = adapter_wgpu_generate_mipmap,
};

/* The adapter's own (heap) copy of a format, to set its caps. */
static struct wined3d_format *wgpu_format_mutable(struct wined3d_adapter *adapter, enum wined3d_format_id id)
{
    const struct wined3d_format *format = wined3d_get_format(adapter, id, 0);

    return format->id == id ? (struct wined3d_format *)format : NULL;
}

/* Formats the core takes (crates/d3dgpu-emu/src/format), and what for. */
static BOOL wgpu_init_format_info(struct wined3d_adapter *adapter)
{
    static const struct
    {
        enum wined3d_format_id id;
        unsigned int caps;
    }
    formats[] =
    {
#define TEX (WINED3D_FORMAT_CAP_TEXTURE | WINED3D_FORMAT_CAP_FILTERING | WINED3D_FORMAT_CAP_BLIT)
#define RT (WINED3D_FORMAT_CAP_RENDERTARGET | WINED3D_FORMAT_CAP_FBO_ATTACHABLE | WINED3D_FORMAT_CAP_POSTPIXELSHADER_BLENDING)
#define DS (WINED3D_FORMAT_CAP_DEPTH_STENCIL | WINED3D_FORMAT_CAP_FBO_ATTACHABLE | WINED3D_FORMAT_CAP_TEXTURE)
        {WINED3DFMT_B8G8R8A8_UNORM, TEX | RT | WINED3D_FORMAT_CAP_SRGB_READ | WINED3D_FORMAT_CAP_SRGB_WRITE},
        {WINED3DFMT_B8G8R8X8_UNORM, TEX | RT | WINED3D_FORMAT_CAP_SRGB_READ | WINED3D_FORMAT_CAP_SRGB_WRITE},
        {WINED3DFMT_R8G8B8A8_UNORM, TEX | RT},
        {WINED3DFMT_R8G8B8X8_UNORM, TEX | RT},
        {WINED3DFMT_B5G6R5_UNORM, TEX},
        {WINED3DFMT_B5G5R5X1_UNORM, TEX},
        {WINED3DFMT_B5G5R5A1_UNORM, TEX},
        {WINED3DFMT_B4G4R4A4_UNORM, TEX},
        {WINED3DFMT_B4G4R4X4_UNORM, TEX},
        {WINED3DFMT_B2G3R3_UNORM, TEX},
        {WINED3DFMT_B2G3R3A8_UNORM, TEX},
        {WINED3DFMT_R10G10B10A2_UNORM, TEX | RT},
        {WINED3DFMT_B10G10R10A2_UNORM, TEX},
        {WINED3DFMT_R16G16_UNORM, TEX},
        {WINED3DFMT_R16G16B16A16_UNORM, TEX},
        {WINED3DFMT_P8_UINT, WINED3D_FORMAT_CAP_TEXTURE | WINED3D_FORMAT_CAP_BLIT},
        {WINED3DFMT_L8_UNORM, TEX},
        {WINED3DFMT_L8A8_UNORM, TEX},
        {WINED3DFMT_L4A4_UNORM, TEX},
        {WINED3DFMT_A8_UNORM, TEX},
        {WINED3DFMT_L16_UNORM, TEX},
        {WINED3DFMT_R8G8_SNORM, TEX},
        {WINED3DFMT_R5G5_SNORM_L6_UNORM, TEX},
        {WINED3DFMT_R8G8_SNORM_L8X8_UNORM, TEX},
        {WINED3DFMT_R8G8B8A8_SNORM, TEX},
        {WINED3DFMT_R16G16_SNORM, TEX},
        {WINED3DFMT_R16G16B16A16_SNORM, TEX},
        {WINED3DFMT_R10G10B10_SNORM_A2_UNORM, TEX},
        {WINED3DFMT_R16_FLOAT, TEX | RT},
        {WINED3DFMT_R16G16_FLOAT, TEX | RT},
        {WINED3DFMT_R16G16B16A16_FLOAT, TEX | RT},
        {WINED3DFMT_R32_FLOAT, WINED3D_FORMAT_CAP_TEXTURE | RT},
        {WINED3DFMT_R32G32_FLOAT, WINED3D_FORMAT_CAP_TEXTURE | RT},
        {WINED3DFMT_R32G32B32A32_FLOAT, WINED3D_FORMAT_CAP_TEXTURE | RT},
        {WINED3DFMT_DXT1, TEX},
        {WINED3DFMT_DXT2, TEX},
        {WINED3DFMT_DXT3, TEX},
        {WINED3DFMT_DXT4, TEX},
        {WINED3DFMT_DXT5, TEX},
        {WINED3DFMT_D16_UNORM, DS},
        {WINED3DFMT_D16_LOCKABLE, DS},
        {WINED3DFMT_X8D24_UNORM, DS},
        {WINED3DFMT_D24_UNORM_S8_UINT, DS},
        {WINED3DFMT_S4X4_UINT_D24_UNORM, DS},
        {WINED3DFMT_S1_UINT_D15_UNORM, DS},
        {WINED3DFMT_D32_UNORM, DS},
        {WINED3DFMT_D32_FLOAT, DS},
        {WINED3DFMT_S8_UINT_D24_FLOAT, DS},
        {WINED3DFMT_INTZ, DS | WINED3D_FORMAT_CAP_FILTERING},
#undef TEX
#undef RT
#undef DS
    };
    static const enum wined3d_format_id vertex_formats[] =
    {
        WINED3DFMT_R32_FLOAT, WINED3DFMT_R32G32_FLOAT, WINED3DFMT_R32G32B32_FLOAT, WINED3DFMT_R32G32B32A32_FLOAT,
        WINED3DFMT_B8G8R8A8_UNORM, WINED3DFMT_R8G8B8A8_UINT, WINED3DFMT_R16G16_SINT, WINED3DFMT_R16G16B16A16_SINT,
        WINED3DFMT_R8G8B8A8_UNORM, WINED3DFMT_R16G16_SNORM, WINED3DFMT_R16G16B16A16_SNORM, WINED3DFMT_R16G16_UNORM,
        WINED3DFMT_R16G16B16A16_UNORM, WINED3DFMT_R10G10B10X2_UINT, WINED3DFMT_R10G10B10X2_SNORM,
        WINED3DFMT_R16G16_FLOAT, WINED3DFMT_R16G16B16A16_FLOAT,
    };
    struct wined3d_format *format;
    unsigned int i, t;

    if (!wined3d_adapter_no3d_init_format_info(adapter))
        return FALSE;

#define get_format_internal(adapter, id) wgpu_format_mutable(adapter, id)

    for (i = 0; i < ARRAY_SIZE(formats); ++i)
    {
        if (!(format = get_format_internal(adapter, formats[i].id)))
            continue;
        for (t = WINED3D_GL_RES_TYPE_TEX_2D; t <= WINED3D_GL_RES_TYPE_TEX_CUBE; ++t)
        {
            unsigned int caps = formats[i].caps;

            /* No 3D depth textures, and render targets only in 2D and cube. */
            if (t == WINED3D_GL_RES_TYPE_TEX_3D)
                caps &= ~(WINED3D_FORMAT_CAP_DEPTH_STENCIL | WINED3D_FORMAT_CAP_RENDERTARGET
                        | WINED3D_FORMAT_CAP_FBO_ATTACHABLE);
            format->caps[t] |= caps;
        }
        format->caps[WINED3D_GL_RES_TYPE_RB] |= formats[i].caps & ~WINED3D_FORMAT_CAP_TEXTURE;
    }
    for (i = 0; i < ARRAY_SIZE(vertex_formats); ++i)
    {
        if ((format = get_format_internal(adapter, vertex_formats[i])))
            format->caps[WINED3D_GL_RES_TYPE_BUFFER] |= WINED3D_FORMAT_CAP_VERTEX_ATTRIBUTE;
    }
    if ((format = get_format_internal(adapter, WINED3DFMT_R16_UINT)))
        format->caps[WINED3D_GL_RES_TYPE_BUFFER] |= WINED3D_FORMAT_CAP_INDEX_BUFFER;
    if ((format = get_format_internal(adapter, WINED3DFMT_R32_UINT)))
        format->caps[WINED3D_GL_RES_TYPE_BUFFER] |= WINED3D_FORMAT_CAP_INDEX_BUFFER;
#undef get_format_internal
    return TRUE;
}

static void wgpu_init_d3d_info(struct wined3d_adapter *adapter, unsigned int wined3d_creation_flags)
{
    struct wined3d_d3d_info *d3d_info = &adapter->d3d_info;
    struct wined3d_vertex_caps vertex_caps;
    struct shader_caps shader_caps;

    adapter->shader_backend->shader_get_caps(adapter, &shader_caps);
    adapter->vertex_pipe->vp_get_caps(adapter, &vertex_caps);
    adapter->fragment_pipe->get_caps(adapter, &d3d_info->ffp_fragment_caps);

    d3d_info->limits.vs_version = shader_caps.vs_version;
    d3d_info->limits.ps_version = shader_caps.ps_version;
    d3d_info->limits.vs_uniform_count = shader_caps.vs_uniform_count;
    d3d_info->limits.ps_uniform_count = shader_caps.ps_uniform_count;
    d3d_info->limits.varying_count = shader_caps.varying_count;
    d3d_info->limits.ffp_vertex_blend_matrices = vertex_caps.max_vertex_blend_matrices;
    d3d_info->limits.active_light_count = vertex_caps.max_active_lights;
    d3d_info->limits.max_rt_count = 4;
    d3d_info->limits.max_clip_distances = WINED3D_MAX_CLIP_DISTANCES;
    /* WebGPU's default limits. */
    d3d_info->limits.texture_size = 8192;
    d3d_info->limits.pointsize_max = 1.0f;
    d3d_info->limits.sample_count = 1;

    d3d_info->wined3d_creation_flags = wined3d_creation_flags;
    d3d_info->emulated_flatshading = vertex_caps.emulated_flatshading;
    d3d_info->ffp_alpha_test = false;
    d3d_info->simple_instancing = true;
    d3d_info->unconditional_npot = true;
    d3d_info->draw_base_vertex_offset = true;
    d3d_info->vertex_bgra = true;
    d3d_info->texture_swizzle = true;
    d3d_info->clip_control = true;
    d3d_info->full_ffp_varyings = true;
    d3d_info->pbo = false;
    d3d_info->feature_level = WINED3D_FEATURE_LEVEL_9_3;
    d3d_info->subpixel_viewport = true;
    d3d_info->fences = false;
    d3d_info->persistent_map = false;
    d3d_info->gpu_push_constants = true;
    d3d_info->ffp_hlsl = true;
    d3d_info->filling_convention_offset = 0.0f;
    d3d_info->multisample_draw_location = WINED3D_LOCATION_TEXTURE_RGB;
}

struct wined3d_adapter *wined3d_adapter_wgpu_create(unsigned int ordinal, unsigned int wined3d_creation_flags)
{
    struct wined3d_gpu_description gpu_description =
    {
        HW_VENDOR_SOFTWARE, CARD_WINE, "WebGPU (d3dgpu)", DRIVER_WINE, 512,
    };
    struct wined3d_adapter *adapter;
    LUID primary_luid, *luid = NULL;

    TRACE("ordinal %u, wined3d_creation_flags %#x.\n", ordinal, wined3d_creation_flags);

    if (__wine_init_unix_call())
    {
        TRACE("No WebGPU host.\n");
        return NULL;
    }
    wgpu_host.version = D3DGPU_VERSION;
    if (WINE_UNIX_CALL(unix_wgpu_open, &wgpu_host) || !wgpu_host.max_batch)
    {
        TRACE("The host has no WebGPU device.\n");
        return NULL;
    }
    wgpu_host.name[sizeof(wgpu_host.name) - 1] = 0;
    if (wgpu_host.name[0])
        gpu_description.description = wgpu_host.name;
    /* There is no thread for the command stream yet: run it on the
     * application's thread. */
    wined3d_settings.cs_multithreaded = 0;

    if (!(adapter = calloc(1, sizeof(*adapter))))
        return NULL;
    if (ordinal == 0 && wined3d_get_primary_adapter_luid(&primary_luid))
        luid = &primary_luid;
    if (!wined3d_adapter_init(adapter, ordinal, luid, &wined3d_adapter_wgpu_ops))
    {
        free(adapter);
        return NULL;
    }
    if (!wgpu_init_format_info(adapter))
    {
        wined3d_adapter_cleanup(adapter);
        free(adapter);
        return NULL;
    }
    if (!wined3d_driver_info_init(&adapter->driver_info, &gpu_description, WINED3D_FEATURE_LEVEL_9_3, 0, 0))
    {
        wined3d_adapter_cleanup(adapter);
        free(adapter);
        return NULL;
    }
    adapter->vram_bytes_used = 0;

    adapter->vertex_pipe = &wgpu_vertex_pipe;
    adapter->fragment_pipe = &wgpu_fragment_pipe;
    adapter->misc_state_template = misc_state_template_wgpu;
    adapter->shader_backend = &wgpu_shader_backend;
    adapter->decoder_ops = &wined3d_null_decoder_ops;
    wgpu_init_d3d_info(adapter, wined3d_creation_flags);

    TRACE("Created WebGPU adapter %p (%s).\n", adapter, debugstr_a(wgpu_host.name));
    return adapter;
}

/* Called when a vertex declaration is destroyed. */
void wined3d_vertex_declaration_wgpu_destroyed(struct wined3d_vertex_declaration *decl)
{
    if (decl->device->adapter->adapter_ops != &wined3d_adapter_wgpu_ops)
        return;
    wgpu_forget_decl(wined3d_device_wgpu(decl->device), decl);
}
