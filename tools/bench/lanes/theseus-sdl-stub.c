/* Headless SDL3 stand-in for Theseus's native host (console programs). */
#include <stdbool.h>
#include <stdint.h>
#include <time.h>
#include <unistd.h>
bool SDL_SetHint(const char *n, const char *v) { return true; }
bool SDL_Init(uint32_t f) { return true; }
uint64_t SDL_GetTicks(void) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return (uint64_t)t.tv_sec * 1000 + t.tv_nsec / 1000000; }
bool SDL_PollEvent(void *e) { return false; }
bool SDL_WaitEvent(void *e) { usleep(10000); return false; }
const char *SDL_GetError(void) { return "headless"; }
#define STUB(name) void *name() { return 0; }
STUB(SDL_GetAudioStreamQueued) STUB(SDL_OpenAudioDeviceStream) STUB(SDL_PutAudioStreamData) STUB(SDL_ResumeAudioStreamDevice)
STUB(SDL_CreateRenderer) STUB(SDL_CreateTexture) STUB(SDL_RenderClear) STUB(SDL_RenderPresent) STUB(SDL_RenderTexture)
STUB(SDL_SetDefaultTextureScaleMode) STUB(SDL_SetRenderDrawColor) STUB(SDL_SetTextureBlendMode) STUB(SDL_UpdateTexture)
STUB(SDL_CreateWindow) STUB(SDL_SetWindowSize)
