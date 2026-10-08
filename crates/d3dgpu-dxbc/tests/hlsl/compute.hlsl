// compile: cs cs_5_0
RWStructuredBuffer<uint> counters : register(u0);
RWByteAddressBuffer raw : register(u1);
RWTexture2D<float4> image : register(u2);
RWBuffer<uint> typed : register(u3);
StructuredBuffer<float4> input : register(t0);
cbuffer Params : register(b0) { uint count; float scale; };
groupshared uint total;
[numthreads(64, 1, 1)]
void cs(uint3 tid : SV_DispatchThreadID, uint3 gid : SV_GroupID, uint gi : SV_GroupIndex, uint3 lid : SV_GroupThreadID) {
    if (gi == 0) total = 0;
    GroupMemoryBarrierWithGroupSync();
    float v = tid.x < count ? input[tid.x].x * scale : 0;
    InterlockedAdd(total, 1);
    GroupMemoryBarrierWithGroupSync();
    if (gi == 0) {
        float sum = 0;
        for (uint i = 0; i < count; i++) sum += input[i].y;
        raw.Store(gid.x * 4, asuint(sum) + total);
        raw.Store2(64, uint2(sum, total));
    }
    uint old;
    InterlockedAdd(counters[0], 1, old);
    InterlockedMax(counters[1], tid.x);
    InterlockedMin(counters[2], tid.x);
    InterlockedOr(counters[3], 1u << (tid.x & 31));
    InterlockedCompareExchange(counters[4], 0, tid.x + 1, old);
    image[tid.xy] = float4(v, lid.x, gid.y, 1);
    typed[tid.x] = old;
    InterlockedAdd(typed[0], 1);
    AllMemoryBarrier();
}
