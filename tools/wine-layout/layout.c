/* Prints the 32-bit layouts of the Windows structures the runtime builds or
 * reads, computed by the compiler from Wine's headers.
 *
 *   tools/wine-layout/gen.sh > runtime/wine/layout.json
 */
#include <stdarg.h>
#include <stddef.h>
#include <windef.h>
#include <winbase.h>
#include <winternl.h>
#include <ddk/wdm.h>

/* No C runtime: plain kernel32 output. */
static void out(const char *s) {
    DWORD n = 0, w;
    while (s[n]) n++;
    WriteFile(GetStdHandle(STD_OUTPUT_HANDLE), s, n, &w, NULL);
}
static void num(unsigned v) {
    char b[12];
    int i = 11;
    b[i] = 0;
    do { b[--i] = '0' + v % 10; v /= 10; } while (v);
    out(b + i);
}
#define F(s, f) (out("    \"" #f "\": "), num((unsigned)offsetof(s, f)), out(",\n"))
#define BEGIN(s) (out("  \"" #s "\": {\n    \"__size\": "), num((unsigned)sizeof(s)), out(",\n"))
#define END() out("    \"__end\": 0\n  },\n")

void start(void) {
    out("{\n");
    BEGIN(TEB);
    F(TEB, Tib.ExceptionList); F(TEB, Tib.StackBase); F(TEB, Tib.StackLimit); F(TEB, Tib.Self);
    F(TEB, EnvironmentPointer); F(TEB, ClientId); F(TEB, ThreadLocalStoragePointer); F(TEB, Peb);
    F(TEB, LastErrorValue); F(TEB, CsrClientThread); F(TEB, WOW32Reserved); F(TEB, CurrentLocale);
    F(TEB, ActivationContextStack); F(TEB, ActivationContextStackPointer); F(TEB, GdiTebBatch);
    F(TEB, StaticUnicodeString); F(TEB, StaticUnicodeBuffer); F(TEB, DeallocationStack);
    F(TEB, TlsSlots); F(TEB, TlsLinks); F(TEB, Vdm); F(TEB, GdiBatchCount); F(TEB, WowTebOffset);
    F(TEB, TlsExpansionSlots); F(TEB, FlsSlots); F(TEB, LastStatusValue); F(TEB, GuaranteedStackBytes);
    END();
    BEGIN(PEB);
    F(PEB, BeingDebugged); F(PEB, ImageBaseAddress); F(PEB, LdrData); F(PEB, ProcessParameters);
    F(PEB, ProcessHeap); F(PEB, FastPebLock); F(PEB, KernelCallbackTable); F(PEB, TlsBitmap);
    F(PEB, TlsBitmapBits); F(PEB, AnsiCodePageData); F(PEB, OemCodePageData); F(PEB, UnicodeCaseTableData);
    F(PEB, NumberOfProcessors); F(PEB, NtGlobalFlag); F(PEB, CriticalSectionTimeout);
    F(PEB, HeapSegmentReserve); F(PEB, HeapSegmentCommit); F(PEB, HeapDeCommitTotalFreeThreshold);
    F(PEB, HeapDeCommitFreeBlockThreshold); F(PEB, NumberOfHeaps); F(PEB, MaximumNumberOfHeaps);
    F(PEB, ProcessHeaps); F(PEB, OSMajorVersion); F(PEB, OSMinorVersion); F(PEB, OSBuildNumber);
    F(PEB, OSPlatformId); F(PEB, ImageSubSystem); F(PEB, ImageSubSystemMajorVersion);
    F(PEB, ImageSubSystemMinorVersion); F(PEB, SessionId); F(PEB, ApiSetMap); F(PEB, LoaderLock);
    F(PEB, ActiveProcessAffinityMask); F(PEB, TlsExpansionBitmap); F(PEB, CloudFileFlags);
    END();
    BEGIN(RTL_USER_PROCESS_PARAMETERS);
    F(RTL_USER_PROCESS_PARAMETERS, AllocationSize); F(RTL_USER_PROCESS_PARAMETERS, Size);
    F(RTL_USER_PROCESS_PARAMETERS, Flags); F(RTL_USER_PROCESS_PARAMETERS, DebugFlags);
    F(RTL_USER_PROCESS_PARAMETERS, ConsoleHandle); F(RTL_USER_PROCESS_PARAMETERS, ConsoleFlags);
    F(RTL_USER_PROCESS_PARAMETERS, hStdInput); F(RTL_USER_PROCESS_PARAMETERS, hStdOutput);
    F(RTL_USER_PROCESS_PARAMETERS, hStdError); F(RTL_USER_PROCESS_PARAMETERS, CurrentDirectory);
    F(RTL_USER_PROCESS_PARAMETERS, DllPath); F(RTL_USER_PROCESS_PARAMETERS, ImagePathName);
    F(RTL_USER_PROCESS_PARAMETERS, CommandLine); F(RTL_USER_PROCESS_PARAMETERS, Environment);
    F(RTL_USER_PROCESS_PARAMETERS, dwX); F(RTL_USER_PROCESS_PARAMETERS, wShowWindow);
    F(RTL_USER_PROCESS_PARAMETERS, WindowTitle); F(RTL_USER_PROCESS_PARAMETERS, Desktop);
    F(RTL_USER_PROCESS_PARAMETERS, ShellInfo); F(RTL_USER_PROCESS_PARAMETERS, RuntimeInfo);
    F(RTL_USER_PROCESS_PARAMETERS, EnvironmentSize); F(RTL_USER_PROCESS_PARAMETERS, ProcessGroupId);
    END();
    BEGIN(CONTEXT);
    F(CONTEXT, ContextFlags); F(CONTEXT, FloatSave); F(CONTEXT, SegGs); F(CONTEXT, SegFs); F(CONTEXT, SegEs);
    F(CONTEXT, SegDs); F(CONTEXT, Edi); F(CONTEXT, Esi); F(CONTEXT, Ebx); F(CONTEXT, Edx); F(CONTEXT, Ecx);
    F(CONTEXT, Eax); F(CONTEXT, Ebp); F(CONTEXT, Eip); F(CONTEXT, SegCs); F(CONTEXT, EFlags);
    F(CONTEXT, Esp); F(CONTEXT, SegSs); F(CONTEXT, ExtendedRegisters);
    END();
    BEGIN(OBJECT_ATTRIBUTES);
    F(OBJECT_ATTRIBUTES, RootDirectory); F(OBJECT_ATTRIBUTES, ObjectName); F(OBJECT_ATTRIBUTES, Attributes);
    END();
    BEGIN(MEMORY_BASIC_INFORMATION);
    F(MEMORY_BASIC_INFORMATION, BaseAddress); F(MEMORY_BASIC_INFORMATION, AllocationBase);
    F(MEMORY_BASIC_INFORMATION, AllocationProtect); F(MEMORY_BASIC_INFORMATION, RegionSize);
    F(MEMORY_BASIC_INFORMATION, State); F(MEMORY_BASIC_INFORMATION, Protect); F(MEMORY_BASIC_INFORMATION, Type);
    END();
    BEGIN(SECTION_IMAGE_INFORMATION);
    F(SECTION_IMAGE_INFORMATION, TransferAddress); F(SECTION_IMAGE_INFORMATION, ZeroBits);
    F(SECTION_IMAGE_INFORMATION, MaximumStackSize); F(SECTION_IMAGE_INFORMATION, CommittedStackSize);
    F(SECTION_IMAGE_INFORMATION, SubSystemType); F(SECTION_IMAGE_INFORMATION, MinorSubsystemVersion);
    F(SECTION_IMAGE_INFORMATION, MajorSubsystemVersion); F(SECTION_IMAGE_INFORMATION, MajorOperatingSystemVersion);
    F(SECTION_IMAGE_INFORMATION, MinorOperatingSystemVersion); F(SECTION_IMAGE_INFORMATION, ImageCharacteristics);
    F(SECTION_IMAGE_INFORMATION, DllCharacteristics); F(SECTION_IMAGE_INFORMATION, Machine);
    F(SECTION_IMAGE_INFORMATION, ImageContainsCode); F(SECTION_IMAGE_INFORMATION, ImageFlags);
    F(SECTION_IMAGE_INFORMATION, LoaderFlags); F(SECTION_IMAGE_INFORMATION, ImageFileSize);
    F(SECTION_IMAGE_INFORMATION, CheckSum);
    END();
    BEGIN(KUSER_SHARED_DATA);
    F(KUSER_SHARED_DATA, TickCountMultiplier); F(KUSER_SHARED_DATA, InterruptTime); F(KUSER_SHARED_DATA, SystemTime);
    F(KUSER_SHARED_DATA, TimeZoneBias); F(KUSER_SHARED_DATA, ImageNumberLow); F(KUSER_SHARED_DATA, NtSystemRoot);
    F(KUSER_SHARED_DATA, NtProductType); F(KUSER_SHARED_DATA, ProductTypeIsValid); F(KUSER_SHARED_DATA, NtMajorVersion);
    F(KUSER_SHARED_DATA, NtMinorVersion); F(KUSER_SHARED_DATA, ProcessorFeatures); F(KUSER_SHARED_DATA, NumberOfPhysicalPages);
    F(KUSER_SHARED_DATA, NtBuildNumber); F(KUSER_SHARED_DATA, ActiveProcessorCount); F(KUSER_SHARED_DATA, TickCount);
    F(KUSER_SHARED_DATA, TickCountQuad); F(KUSER_SHARED_DATA, QpcFrequency); F(KUSER_SHARED_DATA, XState);
    END();
    BEGIN(IO_STATUS_BLOCK);
    F(IO_STATUS_BLOCK, Status); F(IO_STATUS_BLOCK, Information);
    END();
    BEGIN(FILE_BASIC_INFORMATION);
    F(FILE_BASIC_INFORMATION, CreationTime); F(FILE_BASIC_INFORMATION, FileAttributes);
    END();
    BEGIN(FILE_STANDARD_INFORMATION);
    F(FILE_STANDARD_INFORMATION, AllocationSize); F(FILE_STANDARD_INFORMATION, EndOfFile);
    F(FILE_STANDARD_INFORMATION, NumberOfLinks); F(FILE_STANDARD_INFORMATION, DeletePending);
    F(FILE_STANDARD_INFORMATION, Directory);
    END();
    BEGIN(FILE_NETWORK_OPEN_INFORMATION);
    F(FILE_NETWORK_OPEN_INFORMATION, AllocationSize); F(FILE_NETWORK_OPEN_INFORMATION, EndOfFile);
    F(FILE_NETWORK_OPEN_INFORMATION, FileAttributes);
    END();
    BEGIN(SYSTEM_BASIC_INFORMATION);
    F(SYSTEM_BASIC_INFORMATION, PageSize); F(SYSTEM_BASIC_INFORMATION, MmNumberOfPhysicalPages);
    F(SYSTEM_BASIC_INFORMATION, MmLowestPhysicalPage); F(SYSTEM_BASIC_INFORMATION, MmHighestPhysicalPage);
    F(SYSTEM_BASIC_INFORMATION, AllocationGranularity); F(SYSTEM_BASIC_INFORMATION, LowestUserAddress);
    F(SYSTEM_BASIC_INFORMATION, HighestUserAddress); F(SYSTEM_BASIC_INFORMATION, ActiveProcessorsAffinityMask);
    F(SYSTEM_BASIC_INFORMATION, NumberOfProcessors);
    END();
    BEGIN(PROCESS_BASIC_INFORMATION);
    F(PROCESS_BASIC_INFORMATION, ExitStatus); F(PROCESS_BASIC_INFORMATION, PebBaseAddress);
    F(PROCESS_BASIC_INFORMATION, UniqueProcessId);
    END();
    BEGIN(THREAD_BASIC_INFORMATION);
    F(THREAD_BASIC_INFORMATION, ExitStatus); F(THREAD_BASIC_INFORMATION, TebBaseAddress);
    F(THREAD_BASIC_INFORMATION, ClientId); F(THREAD_BASIC_INFORMATION, AffinityMask);
    END();
    out("  \"__end\": 0\n}\n");
    ExitProcess(0);
}
