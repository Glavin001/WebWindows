// compile: vs vs_4_0, ps ps_4_0, vs vs_5_0, ps ps_5_0
cbuffer Frame : register(b0) { float4x4 mvp; float4 tint; int count; uint flags; };
Texture2D tex : register(t0);
SamplerState smp : register(s0);
struct VSIn { float3 pos : POSITION; float2 uv : TEXCOORD0; float4 color : COLOR0; uint id : SV_VertexID; uint inst : SV_InstanceID; };
struct VSOut { float4 pos : SV_Position; float2 uv : TEXCOORD0; float4 color : COLOR0; nointerpolation uint id : BLENDINDICES; };
VSOut vs(VSIn i) {
    VSOut o;
    o.pos = mul(float4(i.pos + float3(i.inst * 0.1, 0, 0), 1), mvp);
    o.uv = i.uv;
    o.color = i.color;
    o.id = i.id;
    return o;
}
float4 ps(VSOut i) : SV_Target {
    float4 c = tex.Sample(smp, i.uv) * tint * i.color;
    for (int k = 0; k < count; k++) c.r += 0.1;
    if ((i.id & flags) != 0) discard;
    return c;
}
