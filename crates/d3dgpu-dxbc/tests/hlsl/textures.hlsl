// compile: ps ps_5_0, ps ps_4_1, vs vs_5_0
Texture2D t2d : register(t0);
Texture2DArray t2da : register(t1);
TextureCube tcube : register(t2);
Texture3D t3d : register(t3);
Texture2D<uint4> tuint : register(t4);
Texture2D<float> tshadow : register(t5);
Texture2DMS<float4> tms : register(t6);
Buffer<float4> buf : register(t7);
StructuredBuffer<float4> sbuf : register(t8);
ByteAddressBuffer rbuf : register(t9);
Texture1D t1d : register(t10);
SamplerState lin : register(s0);
SamplerComparisonState cmp : register(s1);
float4 ps(float4 p : SV_Position, float3 uv : TEXCOORD0) : SV_Target {
    float4 r = t2d.Sample(lin, uv.xy);
    r += t2d.Sample(lin, uv.xy, int2(1, -1));
    r += t2d.SampleLevel(lin, uv.xy, 2);
    r += t2d.SampleBias(lin, uv.xy, 0.5);
    r += t2d.SampleGrad(lin, uv.xy, float2(0.01, 0), float2(0, 0.01));
    r += t2da.Sample(lin, uv);
    r += tcube.Sample(lin, uv);
    r += t3d.Sample(lin, uv);
    r += t1d.Sample(lin, uv.x);
    r += t2d.Load(int3(p.xy, 0));
    r += t2d.Load(int3(p.xy, 1), int2(1, 1));
    r += float4(tuint.Load(int3(p.xy, 0)));
    r += tshadow.SampleCmp(cmp, uv.xy, uv.z);
    r += tshadow.SampleCmpLevelZero(cmp, uv.xy, uv.z);
    r += t2d.Gather(lin, uv.xy);
    r += tms.Load(int2(p.xy), 1);
    r += buf.Load((uint)p.x);
    r += sbuf[(uint)p.y];
    r += asfloat(rbuf.Load4((uint)p.x * 16));
    uint w, h, levels;
    t2d.GetDimensions(0, w, h, levels);
    r.x += w + h + levels;
    return r;
}
float4 vs(float4 pos : POSITION) : SV_Position {
    return pos + t2d.SampleLevel(lin, pos.xy, 0) + buf.Load(0);
}
