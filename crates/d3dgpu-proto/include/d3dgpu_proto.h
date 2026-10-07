/*
 * d3dgpu command stream, for C front ends (wined3d's adapter_wgpu).
 *
 * Mirrors crates/d3dgpu-proto (the Rust crate is the reference; a test there
 * keeps opcodes, constants and fixed command layouts in step with this
 * file).
 *
 * A batch is a struct d3dgpu_batch_header followed by commands. Every
 * command starts with struct d3dgpu_cmd_header; `size` is the total size in
 * bytes including the header and is a multiple of 4. All fields are
 * little-endian 32-bit values. Direct3D 9 enums (D3DFORMAT,
 * D3DRENDERSTATETYPE, D3DPRIMITIVETYPE, ...) are passed with their Direct3D
 * values.
 *
 * Variable-length fields are a struct d3dgpu_data: tag D3DGPU_DATA_INLINE is
 * followed by `len` and the bytes padded to 4; tag D3DGPU_DATA_SHARED by an
 * offset and length into the shared memory region.
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */

#ifndef D3DGPU_PROTO_H
#define D3DGPU_PROTO_H

#include <stdint.h>

#define D3DGPU_MAGIC   0x50473344u /* "D3GP" */
#define D3DGPU_VERSION 1u

#define D3DGPU_DATA_INLINE 0u
#define D3DGPU_DATA_SHARED 1u

enum d3dgpu_op
{
    /* Objects. */
    D3DGPU_OP_CREATE_BUFFER          = 0x0001,
    D3DGPU_OP_DESTROY                = 0x0002,
    D3DGPU_OP_WRITE_BUFFER           = 0x0003,
    D3DGPU_OP_CREATE_TEXTURE         = 0x0004,
    D3DGPU_OP_WRITE_TEXTURE          = 0x0005,
    D3DGPU_OP_CREATE_SHADER          = 0x0006,
    D3DGPU_OP_CREATE_VERTEX_DECL     = 0x0007,
    D3DGPU_OP_SET_PALETTE            = 0x0008,
    /* State. */
    D3DGPU_OP_SET_RENDER_TARGET      = 0x0010,
    D3DGPU_OP_SET_DEPTH_STENCIL      = 0x0011,
    D3DGPU_OP_SET_VIEWPORT           = 0x0012,
    D3DGPU_OP_SET_SCISSOR            = 0x0013,
    D3DGPU_OP_SET_RENDER_STATE       = 0x0014,
    D3DGPU_OP_SET_SAMPLER_STATE      = 0x0015,
    D3DGPU_OP_SET_TEXTURE            = 0x0016,
    D3DGPU_OP_SET_TEXTURE_STAGE_STATE = 0x0017,
    D3DGPU_OP_SET_VERTEX_SHADER      = 0x0018,
    D3DGPU_OP_SET_PIXEL_SHADER       = 0x0019,
    D3DGPU_OP_SET_VERTEX_DECL        = 0x001a,
    D3DGPU_OP_SET_STREAM_SOURCE      = 0x001b,
    D3DGPU_OP_SET_STREAM_FREQ        = 0x001c,
    D3DGPU_OP_SET_INDICES            = 0x001d,
    D3DGPU_OP_SET_SHADER_CONST_F     = 0x001e,
    D3DGPU_OP_SET_SHADER_CONST_I     = 0x001f,
    D3DGPU_OP_SET_SHADER_CONST_B     = 0x0020,
    D3DGPU_OP_SET_CLIP_PLANE         = 0x0021,
    /* Drawing. */
    D3DGPU_OP_CLEAR                  = 0x0030,
    D3DGPU_OP_DRAW                   = 0x0031,
    D3DGPU_OP_DRAW_INDEXED           = 0x0032,
    D3DGPU_OP_DRAW_UP                = 0x0033,
    D3DGPU_OP_DRAW_INDEXED_UP        = 0x0034,
    D3DGPU_OP_STRETCH_RECT           = 0x0035,
    /* Frames and synchronisation. */
    D3DGPU_OP_PRESENT                = 0x0040,
    D3DGPU_OP_SET_GAMMA_RAMP         = 0x0041,
    D3DGPU_OP_READ_TEXTURE           = 0x0050,
    D3DGPU_OP_SIGNAL                 = 0x0051,
    /* Debugging. */
    D3DGPU_OP_MARKER                 = 0x0060,
};

/* CreateBuffer usage. */
#define D3DGPU_BUFFER_VERTEX  0x1u
#define D3DGPU_BUFFER_INDEX   0x2u
#define D3DGPU_BUFFER_DYNAMIC 0x4u

/* CreateTexture usage. */
#define D3DGPU_TEXTURE_RENDER_TARGET 0x1u
#define D3DGPU_TEXTURE_DEPTH_STENCIL 0x2u
#define D3DGPU_TEXTURE_DYNAMIC       0x4u

enum d3dgpu_texture_kind
{
    D3DGPU_TEXTURE_2D     = 0,
    D3DGPU_TEXTURE_CUBE   = 1,
    D3DGPU_TEXTURE_VOLUME = 2,
};

enum d3dgpu_stage
{
    D3DGPU_STAGE_VERTEX = 0,
    D3DGPU_STAGE_PIXEL  = 1,
};

/* Samplers 0..15 are pixel shader samplers, 16..19 vertex texture samplers. */
#define D3DGPU_VERTEX_SAMPLER_BASE 16u

/* Present flags. */
#define D3DGPU_PRESENT_VSYNC 0x1u

struct d3dgpu_batch_header
{
    uint32_t magic;   /* D3DGPU_MAGIC */
    uint32_t version; /* D3DGPU_VERSION */
    uint32_t len;     /* total bytes including this header */
};

struct d3dgpu_cmd_header
{
    uint32_t op;   /* enum d3dgpu_op */
    uint32_t size; /* total bytes including this header */
};

struct d3dgpu_data_shared
{
    uint32_t tag; /* D3DGPU_DATA_SHARED */
    uint32_t offset;
    uint32_t len;
};

struct d3dgpu_rect
{
    int32_t x1, y1, x2, y2; /* right and bottom exclusive */
};

struct d3dgpu_texture_region
{
    uint32_t texture, face, level;
    uint32_t x, y, z;
    uint32_t width, height, depth;
};

/*
 * Fixed-size commands. Commands with variable-length tails are described in
 * comments: their fixed part is the struct, followed by the tail.
 */

struct d3dgpu_cmd_create_buffer
{
    struct d3dgpu_cmd_header h;
    uint32_t id, size, usage;
};

struct d3dgpu_cmd_destroy
{
    struct d3dgpu_cmd_header h;
    uint32_t id;
};

/* Followed by struct d3dgpu_data (the bytes). */
struct d3dgpu_cmd_write_buffer
{
    struct d3dgpu_cmd_header h;
    uint32_t id, offset;
};

struct d3dgpu_cmd_create_texture
{
    struct d3dgpu_cmd_header h;
    uint32_t id, kind, format, width, height, depth, levels, usage;
};

/* Followed by struct d3dgpu_data (texels in the texture's D3DFORMAT). */
struct d3dgpu_cmd_write_texture
{
    struct d3dgpu_cmd_header h;
    struct d3dgpu_texture_region region;
    uint32_t row_pitch, slice_pitch;
};

/* Followed by struct d3dgpu_data (SM1-3 bytecode tokens). */
struct d3dgpu_cmd_create_shader
{
    struct d3dgpu_cmd_header h;
    uint32_t id, stage, hash_lo, hash_hi;
};

/* Followed by `count` D3DVERTEXELEMENT9 (8 bytes each, no D3DDECL_END). */
struct d3dgpu_cmd_create_vertex_decl
{
    struct d3dgpu_cmd_header h;
    uint32_t id, count;
};

struct d3dgpu_cmd_set_palette
{
    struct d3dgpu_cmd_header h;
    uint32_t index;
    uint8_t entries[256][4]; /* PALETTEENTRY: red, green, blue, flags */
};

struct d3dgpu_cmd_set_render_target
{
    struct d3dgpu_cmd_header h;
    uint32_t index, texture, face, level;
};

struct d3dgpu_cmd_set_depth_stencil
{
    struct d3dgpu_cmd_header h;
    uint32_t texture, face, level;
};

struct d3dgpu_cmd_set_viewport
{
    struct d3dgpu_cmd_header h;
    uint32_t x, y, width, height;
    float min_z, max_z;
};

struct d3dgpu_cmd_set_scissor
{
    struct d3dgpu_cmd_header h;
    struct d3dgpu_rect rect;
};

struct d3dgpu_cmd_set_render_state
{
    struct d3dgpu_cmd_header h;
    uint32_t state, value;
};

struct d3dgpu_cmd_set_sampler_state
{
    struct d3dgpu_cmd_header h;
    uint32_t sampler, state, value;
};

struct d3dgpu_cmd_set_texture
{
    struct d3dgpu_cmd_header h;
    uint32_t sampler, texture;
};

struct d3dgpu_cmd_set_texture_stage_state
{
    struct d3dgpu_cmd_header h;
    uint32_t stage, state, value;
};

/* SetVertexShader, SetPixelShader and SetVertexDecl. */
struct d3dgpu_cmd_set_object
{
    struct d3dgpu_cmd_header h;
    uint32_t id;
};

struct d3dgpu_cmd_set_stream_source
{
    struct d3dgpu_cmd_header h;
    uint32_t stream, buffer, offset, stride;
};

struct d3dgpu_cmd_set_stream_freq
{
    struct d3dgpu_cmd_header h;
    uint32_t stream, value;
};

struct d3dgpu_cmd_set_indices
{
    struct d3dgpu_cmd_header h;
    uint32_t buffer, format; /* D3DFMT_INDEX16 or D3DFMT_INDEX32 */
};

/* Followed by count * 4 floats (F), count * 4 int32 (I) or count uint32 (B). */
struct d3dgpu_cmd_set_shader_const
{
    struct d3dgpu_cmd_header h;
    uint32_t stage, start, count;
};

struct d3dgpu_cmd_set_clip_plane
{
    struct d3dgpu_cmd_header h;
    uint32_t index;
    float plane[4];
};

/* Followed by rect_count struct d3dgpu_rect. */
struct d3dgpu_cmd_clear
{
    struct d3dgpu_cmd_header h;
    uint32_t flags, color; /* D3DCLEAR_*, D3DCOLOR */
    float z;
    uint32_t stencil, rect_count;
};

struct d3dgpu_cmd_draw
{
    struct d3dgpu_cmd_header h;
    uint32_t prim, start_vertex, prim_count;
};

struct d3dgpu_cmd_draw_indexed
{
    struct d3dgpu_cmd_header h;
    uint32_t prim;
    int32_t base_vertex;
    uint32_t min_index, num_vertices, start_index, prim_count;
};

/* Followed by struct d3dgpu_data (stream 0 vertices). */
struct d3dgpu_cmd_draw_up
{
    struct d3dgpu_cmd_header h;
    uint32_t prim, prim_count, stride;
};

/* Followed by struct d3dgpu_data (indices), then struct d3dgpu_data (vertices). */
struct d3dgpu_cmd_draw_indexed_up
{
    struct d3dgpu_cmd_header h;
    uint32_t prim, min_index, num_vertices, prim_count, index_format, stride;
};

struct d3dgpu_cmd_stretch_rect
{
    struct d3dgpu_cmd_header h;
    uint32_t src, src_face, src_level;
    struct d3dgpu_rect src_rect;
    uint32_t dst, dst_face, dst_level;
    struct d3dgpu_rect dst_rect;
    uint32_t filter; /* D3DTEXTUREFILTERTYPE */
};

struct d3dgpu_cmd_present
{
    struct d3dgpu_cmd_header h;
    uint32_t texture, window, flags;
};

struct d3dgpu_cmd_set_gamma_ramp
{
    struct d3dgpu_cmd_header h;
    uint32_t window;
    uint16_t ramp[3][256]; /* D3DGAMMARAMP */
};

struct d3dgpu_cmd_read_texture
{
    struct d3dgpu_cmd_header h;
    struct d3dgpu_texture_region region;
    uint32_t dest_offset, row_pitch, slice_pitch;
    uint32_t fence_lo, fence_hi;
};

struct d3dgpu_cmd_signal
{
    struct d3dgpu_cmd_header h;
    uint32_t fence_lo, fence_hi;
};

/* Marker: followed by struct d3dgpu_data (inline UTF-8 text). */

#endif /* D3DGPU_PROTO_H */
