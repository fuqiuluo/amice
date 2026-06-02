// Attribute helpers.
#include "amice_ffi.h"

#include <llvm/IR/Attributes.h>

extern "C" {

// Returns a malloc'd C string for the enum attribute kind; release it with
// amice_free_string.
char *amice_attribute_enum_kind_to_str(llvm::Attribute::AttrKind kind) {
#define ENUM_CASE(name, simple_name) case name: str = #simple_name; break;
    std::string str;
    switch (kind) {
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 15)
        ENUM_CASE(llvm::Attribute::AllocAlign, AllocAlign) // llvm 15
        ENUM_CASE(llvm::Attribute::AllocatedPointer, AllocatedPointer) // llvm 15
#endif
        ENUM_CASE(llvm::Attribute::AlwaysInline, AlwaysInline)
        ENUM_CASE(llvm::Attribute::Builtin, Builtin)
        ENUM_CASE(llvm::Attribute::NoUndef, NoUndef)
        ENUM_CASE(llvm::Attribute::Cold, Cold)
        ENUM_CASE(llvm::Attribute::Convergent, Convergent)
        ENUM_CASE(llvm::Attribute::Hot, Hot)
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 14)
        ENUM_CASE(llvm::Attribute::DisableSanitizerInstrumentation, DisableSanitizerInstrumentation) // llvm 14
#endif
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 15)
        ENUM_CASE(llvm::Attribute::FnRetThunkExtern, FnRetThunkExtern) // llvm 15
#endif
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 19) && (LLVM_VERSION_MINOR >= 1)
        ENUM_CASE(llvm::Attribute::HybridPatchable, HybridPatchable) // llvm 19
#endif
        ENUM_CASE(llvm::Attribute::InlineHint, InlineHint)
        ENUM_CASE(llvm::Attribute::InReg, InReg)
        ENUM_CASE(llvm::Attribute::JumpTable, JumpTable)
        ENUM_CASE(llvm::Attribute::MinSize, MinSize)
        ENUM_CASE(llvm::Attribute::Naked, Naked)
        ENUM_CASE(llvm::Attribute::Nest, Nest)
        ENUM_CASE(llvm::Attribute::NoAlias, NoAlias)
        ENUM_CASE(llvm::Attribute::NoBuiltin, NoBuiltin)
        ENUM_CASE(llvm::Attribute::NoCallback, NoCallback)
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 20)
        ENUM_CASE(llvm::Attribute::NoDivergenceSource, NoDivergenceSource) // llvm 20
#endif
        ENUM_CASE(llvm::Attribute::NoDuplicate, NoDuplicate)
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 20)
        ENUM_CASE(llvm::Attribute::NoExt, NoExt) // llvm 20
#endif
        ENUM_CASE(llvm::Attribute::NoFree, NoFree)
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 18)
        ENUM_CASE(llvm::Attribute::DeadOnUnwind, DeadOnUnwind) // llvm 18
#endif
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 21) && (LLVM_VERSION_MINOR >= 1)
        ENUM_CASE(llvm::Attribute::DeadOnReturn, DeadOnReturn)
#endif
        ENUM_CASE(llvm::Attribute::NoImplicitFloat, NoImplicitFloat)
        ENUM_CASE(llvm::Attribute::NoInline, NoInline)
        ENUM_CASE(llvm::Attribute::NonLazyBind, NonLazyBind)
        ENUM_CASE(llvm::Attribute::NoMerge, NoMerge)
        ENUM_CASE(llvm::Attribute::NonNull, NonNull)
        ENUM_CASE(llvm::Attribute::NoRecurse, NoRecurse)
        ENUM_CASE(llvm::Attribute::NoRedZone, NoRedZone)
        ENUM_CASE(llvm::Attribute::NoReturn, NoReturn)
        ENUM_CASE(llvm::Attribute::NoSync, NoSync)
        ENUM_CASE(llvm::Attribute::NoCfCheck, NoCfCheck)
        ENUM_CASE(llvm::Attribute::NoProfile, NoProfile)
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 16)
        ENUM_CASE(llvm::Attribute::SkipProfile, SkipProfile) // llvm 16
#endif
        ENUM_CASE(llvm::Attribute::NoUnwind, NoUnwind)
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 15)
        ENUM_CASE(llvm::Attribute::NoSanitizeBounds, NoSanitizeBounds) // llvm 15
#endif
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 13)
        ENUM_CASE(llvm::Attribute::NoSanitizeCoverage, NoSanitizeCoverage) // llvm 13
#endif
        ENUM_CASE(llvm::Attribute::NullPointerIsValid, NullPointerIsValid)
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 18)
        ENUM_CASE(llvm::Attribute::OptimizeForDebugging, OptimizeForDebugging) // llvm18
#endif
        ENUM_CASE(llvm::Attribute::OptForFuzzing, OptForFuzzing)
        ENUM_CASE(llvm::Attribute::OptimizeForSize, OptimizeForSize)
        ENUM_CASE(llvm::Attribute::OptimizeNone, OptimizeNone)
        ENUM_CASE(llvm::Attribute::ReadNone, ReadNone)
        ENUM_CASE(llvm::Attribute::ReadOnly, ReadOnly)
        ENUM_CASE(llvm::Attribute::Returned, Returned)
        ENUM_CASE(llvm::Attribute::ImmArg, ImmArg)
        ENUM_CASE(llvm::Attribute::ReturnsTwice, ReturnsTwice)
        ENUM_CASE(llvm::Attribute::SafeStack, SafeStack)
        ENUM_CASE(llvm::Attribute::ShadowCallStack, ShadowCallStack)
        ENUM_CASE(llvm::Attribute::SExt, SExt)
        ENUM_CASE(llvm::Attribute::Speculatable, Speculatable)
        ENUM_CASE(llvm::Attribute::StackProtect, StackProtect)
        ENUM_CASE(llvm::Attribute::StackProtectReq, StackProtectReq)
        ENUM_CASE(llvm::Attribute::StackProtectStrong, StackProtectStrong)
        ENUM_CASE(llvm::Attribute::StrictFP, StrictFP)
        ENUM_CASE(llvm::Attribute::SanitizeAddress, SanitizeAddress)
        ENUM_CASE(llvm::Attribute::SanitizeThread, SanitizeThread)
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 20)
        ENUM_CASE(llvm::Attribute::SanitizeType, SanitizeType) // llvm 20
#endif
        ENUM_CASE(llvm::Attribute::SanitizeMemory, SanitizeMemory)
        ENUM_CASE(llvm::Attribute::SanitizeHWAddress, SanitizeHWAddress)
        ENUM_CASE(llvm::Attribute::SanitizeMemTag, SanitizeMemTag)
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 19) && (LLVM_VERSION_MINOR >= 1)
        ENUM_CASE(llvm::Attribute::SanitizeNumericalStability, SanitizeNumericalStability) // llvm 19
#endif
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 20)
        ENUM_CASE(llvm::Attribute::SanitizeRealtime, SanitizeRealtime) // llvm 20
        ENUM_CASE(llvm::Attribute::SanitizeRealtimeBlocking, SanitizeRealtimeBlocking) // llvm 20
#endif
        ENUM_CASE(llvm::Attribute::SpeculativeLoadHardening, SpeculativeLoadHardening)
        ENUM_CASE(llvm::Attribute::SwiftError, SwiftError)
        ENUM_CASE(llvm::Attribute::SwiftSelf, SwiftSelf)
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 13)
        ENUM_CASE(llvm::Attribute::SwiftAsync, SwiftAsync) // llvm 13
#endif
        ENUM_CASE(llvm::Attribute::WillReturn, WillReturn)
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 18)
        ENUM_CASE(llvm::Attribute::Writable, Writable) // llvm18
#endif
        ENUM_CASE(llvm::Attribute::WriteOnly, WriteOnly)
        ENUM_CASE(llvm::Attribute::ZExt, ZExt)
        ENUM_CASE(llvm::Attribute::MustProgress, MustProgress)
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 15)
        ENUM_CASE(llvm::Attribute::PresplitCoroutine, PresplitCoroutine) // llvm 15
#endif
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 18)
        ENUM_CASE(llvm::Attribute::CoroDestroyOnlyWhenComplete, CoroDestroyOnlyWhenComplete) // llvm18
#endif
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 20)
        ENUM_CASE(llvm::Attribute::CoroElideSafe, CoroElideSafe) // llvm 20
#endif
        default: str = "unknown"; break;
    }
    char *cstr = (char *)malloc(str.size() + 1);
    strcpy(cstr, str.c_str());
    cstr[str.size()] = '\0';
    return cstr;
#undef ENUM_CASE
}

}
