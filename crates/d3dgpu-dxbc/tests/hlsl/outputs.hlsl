// compile: vs vs_5_0, ps ps_5_0, ps2 ps_5_0, vs vs_4_0
struct V2P {
    float4 pos : SV_Position;
    centroid float2 uv : TEXCOORD0;
    noperspective float depth : TEXCOORD1;
    nointerpolation uint4 ids : IDS;

};
V2P vs(float4 pos : POSITION, uint4 ids : BLENDINDICES, float2 uv : TEXCOORD0) {
    V2P o;
    o.pos = pos;
    o.uv = uv;
    o.depth = pos.z;
    o.ids = ids;

    return o;
}
struct PSOut { float4 c0 : SV_Target0; uint4 c1 : SV_Target1; int c2 : SV_Target2; float depth : SV_Depth; };
PSOut ps(V2P i, bool front : SV_IsFrontFace) {
    PSOut o;
    o.c0 = float4(i.uv, i.depth, front ? 1 : 0);
    o.c1 = i.ids;
    o.c2 = -1;
    o.depth = i.pos.z * 0.5;
    return o;
}
float4 ps2(float4 pos : SV_Position, uint s : SV_SampleIndex) : SV_Target {
    return float4(pos.w, s, 0, 1);
}
