// Shaders of the Direct3D 11 performance scene (crates/d3dgpu-scenes/src/perf.rs).
// compile: vs_perf vs_4_0, ps_perf ps_4_0
cbuffer PerDraw : register(b0) { float4 offset; float4 color; };
Texture2D tex : register(t0);
SamplerState smp : register(s0);
struct VI { float3 pos : POSITION; float2 uv : TEXCOORD; };
struct VO { float4 pos : SV_Position; float2 uv : TEXCOORD; float4 col : COLOR; };
VO vs_perf(VI i) { VO o; o.pos = float4(i.pos.xy + offset.xy, i.pos.z, 1); o.uv = i.uv; o.col = color; return o; }
float4 ps_perf(VO i) : SV_Target { return tex.Sample(smp, i.uv) * i.col; }
