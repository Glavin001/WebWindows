// Shaders of the animated Direct3D 9 demos (crates/d3dgpu-scenes/src/demos.rs),
// compiled to shader model 2/3 bytecode.
// compile: vs9_mesh vs_3_0, ps9_mesh ps_3_0, vs9_sprite vs_2_0, ps9_sprite ps_2_0

row_major float4x4 view_proj : register(c0);
row_major float4x4 world : register(c4);
float4 light_dir : register(c8);
float4 eye : register(c9);
float4 color : register(c10);
float4 screen : register(c11); // xy: 2 / size
sampler2D tex : register(s0);

struct MeshOut9 { float4 pos : POSITION; float3 wpos : TEXCOORD0; float3 n : TEXCOORD1; float2 uv : TEXCOORD2; };
MeshOut9 vs9_mesh(float3 pos : POSITION, float3 n : NORMAL, float2 uv : TEXCOORD0) {
    MeshOut9 o;
    float4 w = mul(float4(pos, 1), world);
    o.pos = mul(w, view_proj);
    o.wpos = w.xyz;
    o.n = mul(n, (float3x3)world);
    o.uv = uv;
    return o;
}
float4 ps9_mesh(MeshOut9 i) : COLOR {
    float3 n = normalize(i.n);
    float3 l = -normalize(light_dir.xyz);
    float3 v = normalize(eye.xyz - i.wpos);
    float3 h = normalize(l + v);
    float d = saturate(dot(n, l));
    float s = pow(saturate(dot(n, h)), 32);
    float3 albedo = tex2D(tex, i.uv).rgb * color.rgb;
    return float4(albedo * (0.15 + 0.85 * d) + s * 0.5, 1);
}

struct SpriteOut9 { float4 pos : POSITION; float4 c : COLOR0; float2 uv : TEXCOORD0; };
SpriteOut9 vs9_sprite(float2 pos : POSITION, float4 c : COLOR0, float2 uv : TEXCOORD0) {
    SpriteOut9 o;
    o.pos = float4(pos * screen.xy + float2(-1, 1), 0.5, 1);
    o.c = c;
    o.uv = uv;
    return o;
}
float4 ps9_sprite(SpriteOut9 i) : COLOR {
    return tex2D(tex, i.uv) * i.c;
}
