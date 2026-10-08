// compile: ps ps_5_0, ps ps_4_0
cbuffer C : register(b1) { float4 a; float4 b; int4 ia; uint4 ua; float4 arr[8]; };
static const float4 table[4] = { float4(1, 0, 0, 1), float4(0, 1, 0, 1), float4(0, 0, 1, 1), float4(1, 1, 1, 1) };
float4 ps(float4 p : SV_Position, float2 uv : TEXCOORD0, nointerpolation int sel : SEL, noperspective float3 n : NORMAL) : SV_Target {
    float4 r = 0;
    r += sin(a) + cos(b) + frac(a) + round(b) + floor(a) + ceil(b) + trunc(a);
    r += rsqrt(abs(a) + 1) + rcp(b + 2) + sqrt(abs(a)) + exp2(a) + log2(abs(b) + 1) + pow(abs(a), b);
    r += min(a, b) + max(a, b) + saturate(a) + lerp(a, b, 0.25) + step(a, b) + smoothstep(0, 1, a);
    r.x += dot(a, b) + dot(a.xyz, b.xyz) + dot(a.xy, b.xy);
    r.xyz += cross(a.xyz, b.xyz) + normalize(n);
    r += ddx(uv.x) + ddy(uv.y);
    int4 iv = ia * 3 - (ia >> 1) + (ia << 2);
    uint4 uv4 = ua / 7 + ua % 5 + (ua ^ 0x55) + (ua & 0xff) + (ua | 3) + ~ua + countbits(ua) + firstbithigh(ua) + firstbitlow(ua);
    iv += ia / 3 + ia % 4 + firstbithigh(ia) + abs(ia) + max(ia, -ia) + min(ia, 2);
    r += float4(iv) + float4(uv4) + asfloat(asuint(a) ^ 0x80000000);
    r.x += f16tof32(f32tof16(a.x));
    r += (a > b) ? a : b;
    if (any(a < 0) && all(b > 0)) r *= 2;
    switch (sel) {
        case 0: r.x += 1; break;
        case 1:
        case 2: r.y += 2; break;
        default: r.z += 3; break;
    }
    [loop] for (int i = 0; i < ia.x; i++) {
        if (i == ia.y) continue;
        if (i > 6) break;
        r += arr[i & 7];
    }
    float4 local[4];
    for (int j = 0; j < 4; j++) local[j] = a * j;
    r += local[ia.z & 3] + table[ua.x & 3];
    return r;
}
