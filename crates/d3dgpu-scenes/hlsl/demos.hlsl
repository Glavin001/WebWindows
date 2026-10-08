// Shaders of the animated Direct3D 11 demos (crates/d3dgpu-scenes/src/demos.rs).
// compile: vs_mesh vs_4_0, ps_mesh ps_4_0, vs_inst vs_4_0, cs_particles cs_5_0, vs_particles vs_5_0, ps_particles ps_4_0, vs_post vs_4_0, ps_bright ps_4_0, ps_blur ps_4_0, ps_composite ps_4_0, vs_shadow vs_4_0, vs_lit vs_4_0, ps_lit ps_4_0

cbuffer Frame : register(b0) {
    row_major float4x4 view_proj;
    row_major float4x4 light_view_proj;
    float4 light_dir;   // xyz: direction the light travels
    float4 eye;
    float4 params;      // x: time, y: instance grid side, z: aspect
};
cbuffer Object : register(b1) {
    row_major float4x4 world;
    float4 color;
};
Texture2D tex : register(t0);
SamplerState smp : register(s0);

struct MeshIn { float3 pos : POSITION; float3 normal : NORMAL; float2 uv : TEXCOORD; };
struct MeshOut {
    float4 pos : SV_Position;
    float3 wpos : WORLDPOS;
    float3 normal : NORMAL;
    float2 uv : TEXCOORD;
    float4 color : COLOR;
};

float3 shade(float3 wpos, float3 n, float3 albedo) {
    n = normalize(n);
    float3 l = -normalize(light_dir.xyz);
    float3 v = normalize(eye.xyz - wpos);
    float3 h = normalize(l + v);
    float d = saturate(dot(n, l));
    float s = pow(saturate(dot(n, h)), 32);
    return albedo * (0.15 + 0.85 * d) + s * 0.5;
}

MeshOut vs_mesh(MeshIn i) {
    MeshOut o;
    float4 w = mul(float4(i.pos, 1), world);
    o.pos = mul(w, view_proj);
    o.wpos = w.xyz;
    o.normal = mul(i.normal, (float3x3)world);
    o.uv = i.uv;
    o.color = color;
    return o;
}

float4 ps_mesh(MeshOut i) : SV_Target {
    float3 albedo = tex.Sample(smp, i.uv).rgb * i.color.rgb;
    return float4(shade(i.wpos, i.normal, albedo), 1);
}

// Instanced cubes: the transform comes from SV_InstanceID and the time.
MeshOut vs_inst(MeshIn i, uint id : SV_InstanceID) {
    uint side = (uint)params.y;
    float3 cell = float3(id % side, (id / side) % side, id / (side * side));
    float3 centre = (cell - (side - 1) * 0.5) * 2.0;
    float a = params.x * (0.5 + (id % 7) * 0.15) + id * 0.37;
    float s = sin(a), c = cos(a);
    float3 p = i.pos * 0.5;
    p = float3(p.x * c - p.z * s, p.y, p.x * s + p.z * c);
    float3 n = float3(i.normal.x * c - i.normal.z * s, i.normal.y, i.normal.x * s + i.normal.z * c);
    MeshOut o;
    o.wpos = p + centre;
    o.pos = mul(float4(o.wpos, 1), view_proj);
    o.normal = n;
    o.uv = i.uv;
    float h = frac(id * 0.618034);
    o.color = float4(saturate(abs(h * 6 - 3) - 1), saturate(2 - abs(h * 6 - 2)), saturate(2 - abs(h * 6 - 4)), 1);
    return o;
}

// Particles: positions (xyz, life) and velocities in structured buffers,
// advanced by a compute shader and drawn as camera-facing quads.
RWStructuredBuffer<float4> pos_rw : register(u0);
RWStructuredBuffer<float4> vel_rw : register(u1);
cbuffer Sim : register(b0) { float4 sim; }; // x: dt, y: time, z: count

float rand(inout uint s) {
    s = s * 747796405u + 2891336453u;
    uint w = ((s >> ((s >> 28) + 4)) ^ s) * 277803737u;
    return ((w >> 22) ^ w) / 4294967295.0;
}

[numthreads(64, 1, 1)]
void cs_particles(uint3 id : SV_DispatchThreadID) {
    uint i = id.x;
    if (i >= (uint)sim.z)
        return;
    float4 p = pos_rw[i];
    float4 v = vel_rw[i];
    v.y -= 4.0 * sim.x;
    p.xyz += v.xyz * sim.x;
    p.w -= sim.x;
    if (p.w <= 0 || p.y < -3) {
        uint s = i * 9781u + (uint)(sim.y * 1000.0) * 6271u;
        float a = rand(s) * 6.2831853;
        float r = rand(s) * 0.6;
        p = float4(cos(a) * r * 0.2, -2.5, sin(a) * r * 0.2, 1.5 + rand(s) * 2.0);
        v = float4(cos(a) * r, 5.0 + rand(s) * 2.5, sin(a) * r, 0);
    }
    pos_rw[i] = p;
    vel_rw[i] = v;
}

StructuredBuffer<float4> particles : register(t0);
struct PartOut { float4 pos : SV_Position; float2 uv : TEXCOORD; float4 color : COLOR; };
PartOut vs_particles(uint vid : SV_VertexID) {
    float4 p = particles[vid / 6];
    uint corner = vid % 6;
    float2 q = float2(corner == 1 || corner == 2 || corner == 4 ? 1 : -1, corner == 2 || corner == 4 || corner == 5 ? 1 : -1);
    PartOut o;
    o.pos = mul(float4(p.xyz, 1), view_proj);
    o.pos.xy += q * float2(1 / params.z, 1) * 0.03 * o.pos.w;
    o.uv = q;
    float life = saturate(p.w / 3.0);
    o.color = float4(1.0, 0.35 + 0.6 * life, 0.1 + 0.3 * life * life, 1) * (0.25 * saturate(p.w));
    return o;
}
float4 ps_particles(PartOut i) : SV_Target {
    float a = saturate(1 - dot(i.uv, i.uv));
    return i.color * a * a;
}

// Post-processing: a full-screen triangle and the bloom passes.
struct PostOut { float4 pos : SV_Position; float2 uv : TEXCOORD; };
PostOut vs_post(uint id : SV_VertexID) {
    PostOut o;
    float2 p = float2((id << 1) & 2, id & 2);
    o.pos = float4(p * float2(2, -2) + float2(-1, 1), 0, 1);
    o.uv = p;
    return o;
}
Texture2D hdr : register(t0);
Texture2D bloom : register(t1);
cbuffer Post : register(b0) { float4 post; }; // xy: blur step in uv, z: bloom strength
float4 ps_bright(PostOut i) : SV_Target {
    float3 c = hdr.Sample(smp, i.uv).rgb;
    return float4(max(c - 1.0, 0), 1);
}
float4 ps_blur(PostOut i) : SV_Target {
    static const float w[5] = { 0.227027, 0.1945946, 0.1216216, 0.054054, 0.016216 };
    float3 c = hdr.Sample(smp, i.uv).rgb * w[0];
    for (int k = 1; k < 5; k++) {
        c += hdr.Sample(smp, i.uv + post.xy * k).rgb * w[k];
        c += hdr.Sample(smp, i.uv - post.xy * k).rgb * w[k];
    }
    return float4(c, 1);
}
float4 ps_composite(PostOut i) : SV_Target {
    float3 c = hdr.Sample(smp, i.uv).rgb + bloom.Sample(smp, i.uv).rgb * post.z;
    c = c / (1 + c);
    return float4(pow(c, 1 / 2.2), 1);
}

// Shadow mapping: depth from the light, then a 3x3 PCF lookup.
float4 vs_shadow(float3 pos : POSITION) : SV_Position {
    return mul(mul(float4(pos, 1), world), light_view_proj);
}
struct LitOut {
    float4 pos : SV_Position;
    float3 wpos : WORLDPOS;
    float3 normal : NORMAL;
    float2 uv : TEXCOORD;
    float4 color : COLOR;
    float4 lpos : LIGHTPOS;
};
LitOut vs_lit(MeshIn i) {
    LitOut o;
    float4 w = mul(float4(i.pos, 1), world);
    o.pos = mul(w, view_proj);
    o.wpos = w.xyz;
    o.normal = mul(i.normal, (float3x3)world);
    o.uv = i.uv;
    o.color = color;
    o.lpos = mul(w, light_view_proj);
    return o;
}
Texture2D<float> shadow_map : register(t1);
SamplerComparisonState shadow_cmp : register(s1);
float4 ps_lit(LitOut i) : SV_Target {
    float3 l = i.lpos.xyz / i.lpos.w;
    float2 uv = l.xy * float2(0.5, -0.5) + 0.5;
    float lit = 0;
    for (int y = -1; y <= 1; y++)
        for (int x = -1; x <= 1; x++)
            lit += shadow_map.SampleCmpLevelZero(shadow_cmp, uv + float2(x, y) / 1024.0, l.z - 0.002);
    lit /= 9;
    float3 albedo = tex.Sample(smp, i.uv * 4).rgb * i.color.rgb;
    float3 n = normalize(i.normal);
    float d = saturate(dot(n, -normalize(light_dir.xyz)));
    return float4(albedo * (0.2 + 0.8 * d * lit), 1);
}
