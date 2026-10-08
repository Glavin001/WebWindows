// Shaders of the Direct3D 11 scenes (crates/d3dgpu-scenes/src/scenes11.rs).
// Compiled to DXBC with tools/dxbc/compile.sh crates/d3dgpu-scenes/hlsl.
// compile: vs_color vs_4_0, ps_color ps_4_0, vs_tex vs_4_0, ps_tex ps_4_0, ps_tex_lod ps_4_0, vs_cb vs_4_0, ps_cb ps_4_0, vs_fullscreen vs_4_0, ps_svpos ps_4_0, vs_w2 vs_4_0, vs_inst vs_4_0, cs_fill cs_5_0, ps_buffer ps_4_0, vs_depth vs_4_0, ps_shadow ps_4_0, ps_mrt ps_4_0, ps_uint ps_4_0

struct VI { float3 pos : POSITION; float4 col : COLOR; };
struct VO { float4 pos : SV_Position; float4 col : COLOR; };
VO vs_color(VI i) { VO o; o.pos = float4(i.pos, 1); o.col = i.col; return o; }
float4 ps_color(VO i) : SV_Target { return i.col; }

struct TI { float3 pos : POSITION; float2 uv : TEXCOORD; };
struct TO { float4 pos : SV_Position; float2 uv : TEXCOORD; };
TO vs_tex(TI i) { TO o; o.pos = float4(i.pos, 1); o.uv = i.uv; return o; }
Texture2D tex : register(t0);
SamplerState smp : register(s0);
float4 ps_tex(TO i) : SV_Target { return tex.Sample(smp, i.uv); }
cbuffer Lod : register(b0) { float4 lod; };
float4 ps_tex_lod(TO i) : SV_Target { return tex.SampleLevel(smp, i.uv, lod.x); }

// Position scaled and offset by b0; colour b0.color * b1.tint.
cbuffer Object : register(b0) { float4 offset_scale; float4 color; };
cbuffer Frame : register(b1) { float4 tint; };
float4 vs_cb(float3 pos : POSITION) : SV_Position { return float4(pos.xy * offset_scale.zw + offset_scale.xy, pos.z, 1); }
float4 ps_cb(float4 p : SV_Position) : SV_Target { return color * tint; }

// A triangle covering the screen from SV_VertexID 0, 1, 2 alone.
float4 vs_fullscreen(uint id : SV_VertexID) : SV_Position {
    float2 p = float2((id << 1) & 2, id & 2);
    return float4(p * float2(2, -2) + float2(-1, 1), 0, 1);
}

float4 ps_svpos(float4 p : SV_Position) : SV_Target { return float4(p.x / 64, p.y / 64, p.z, p.w / 4); }
float4 vs_w2(float3 pos : POSITION) : SV_Position { return float4(pos * 2, 2); }

struct II { float3 pos : POSITION; float2 offset : OFFSET; float4 col : COLOR; uint iid : SV_InstanceID; };
VO vs_inst(II i) { VO o; o.pos = float4(i.pos.xy + i.offset, i.pos.z, 1); o.col = float4(i.col.rg, i.iid, 1); return o; }

RWTexture2D<float4> out_tex : register(u0);
RWStructuredBuffer<uint> out_buf : register(u1);
cbuffer Params : register(b0) { float4 fill; };
[numthreads(8, 8, 1)]
void cs_fill(uint3 id : SV_DispatchThreadID) {
    out_tex[id.xy] = float4(id.x / 16.0, id.y / 16.0, fill.b, 1);
    if (id.x < 4 && id.y == 0)
        out_buf[id.x] = id.x * 3 + 1;
}

Buffer<float4> colors : register(t0);
float4 ps_buffer(float4 p : SV_Position) : SV_Target { return colors[(uint)p.x / 16]; }

float4 vs_depth(float3 pos : POSITION) : SV_Position { return float4(pos, 1); }
Texture2D<float> shadow : register(t0);
SamplerComparisonState cmp : register(s0);
float4 ps_shadow(TO i) : SV_Target { float c = shadow.SampleCmpLevelZero(cmp, i.uv, 0.5); return float4(c, c, c, 1); }

struct MO { float4 a : SV_Target0; float4 b : SV_Target1; };
MO ps_mrt(VO i) { MO o; o.a = i.col; o.b = float4(1 - i.col.rgb, 1); return o; }

uint ps_uint(float4 p : SV_Position) : SV_Target { return (uint)p.x + (uint)p.y * 1000; }
