use crate::ffi;
use std::ffi::CStr;
use std::str::FromStr;
use strum::{IntoEnumIterator, ParseError};
use strum_macros::{Display, EnumIter, EnumString};

// translate from https://github.com/llvm/llvm-project/blob/main/llvm/include/llvm/IR/Attributes.td
#[derive(Debug, Copy, Clone, EnumString, PartialEq, Eq, Hash, Display, EnumIter)]
pub enum AttributeEnumKind {
    /// Parameter of a function that tells us the alignment of an allocation, as in
    /// aligned_alloc and aligned ::operator::new.
    // def AllocAlign: EnumAttr<"allocalign", IntersectAnd, [ParamAttr]>;
    AllocAlign,

    /// Parameter is the pointer to be manipulated by the allocator function.
    // def AllocatedPointer : EnumAttr<"allocptr", IntersectAnd, [ParamAttr]>;
    AllocatedPointer,

    /// inline=always.
    // def AlwaysInline : EnumAttr<"alwaysinline", IntersectPreserve, [FnAttr]>;
    AlwaysInline,

    /// Callee is recognized as a builtin, despite nobuiltin attribute on its
    /// declaration.
    // def Builtin : EnumAttr<"builtin", IntersectPreserve, [FnAttr]>;
    Builtin,

    /// Parameter or return value may not contain uninitialized or poison bits.
    // def NoUndef : EnumAttr<"noundef", IntersectAnd, [ParamAttr, RetAttr]>;
    NoUndef,

    /// Marks function as being in a cold path.
    // def Cold : EnumAttr<"cold", IntersectAnd, [FnAttr]>;
    Cold,

    /// Can only be moved to control-equivalent blocks.
    /// NB: Could be IntersectCustom with "or" handling.
    // def Convergent : EnumAttr<"convergent", IntersectPreserve, [FnAttr]>;
    Convergent,

    /// Marks function as being in a hot path and frequently called.
    // def Hot: EnumAttr<"hot", IntersectAnd, [FnAttr]>;
    Hot,

    /// Do not instrument function with sanitizers.
    // def DisableSanitizerInstrumentation: EnumAttr<"disable_sanitizer_instrumentation", IntersectPreserve, [FnAttr]>;
    DisableSanitizerInstrumentation,

    /// Whether to keep return instructions, or replace with a jump to an external
    /// symbol.
    // def FnRetThunkExtern : EnumAttr<"fn_ret_thunk_extern", IntersectPreserve, [FnAttr]>;
    FnRetThunkExtern,

    /// Function has a hybrid patchable thunk.
    // def HybridPatchable : EnumAttr<"hybrid_patchable", IntersectPreserve, [FnAttr]>;
    HybridPatchable,

    /// Source said inlining was desirable.
    // def InlineHint : EnumAttr<"inlinehint", IntersectAnd, [FnAttr]>;
    InlineHint,

    /// Force argument to be passed in register.
    // def InReg : EnumAttr<"inreg", IntersectPreserve, [ParamAttr, RetAttr]>;
    InReg,

    /// Build jump-instruction tables and replace refs.
    // def JumpTable : EnumAttr<"jumptable", IntersectPreserve, [FnAttr]>;
    JumpTable,
    /// Function must be optimized for size first.
    // def MinSize : EnumAttr<"minsize", IntersectPreserve, [FnAttr]>;
    MinSize,

    /// Naked function.
    // def Naked : EnumAttr<"naked", IntersectPreserve, [FnAttr]>;
    Naked,

    /// Nested function static chain.
    // def Nest : EnumAttr<"nest", IntersectPreserve, [ParamAttr]>;
    Nest,

    /// Considered to not alias after call.
    // def NoAlias : EnumAttr<"noalias", IntersectAnd, [ParamAttr, RetAttr]>;
    NoAlias,

    /// Callee isn't recognized as a builtin.
    // def NoBuiltin : EnumAttr<"nobuiltin", IntersectPreserve, [FnAttr]>;
    NoBuiltin,

    /// Function cannot enter into caller's translation unit.
    // def NoCallback : EnumAttr<"nocallback", IntersectAnd, [FnAttr]>;
    NoCallback,

    /// Function is not a source of divergence.
    // def NoDivergenceSource : EnumAttr<"nodivergencesource", IntersectAnd, [FnAttr]>;
    NoDivergenceSource,

    /// Call cannot be duplicated.
    // def NoDuplicate : EnumAttr<"noduplicate", IntersectPreserve, [FnAttr]>;
    NoDuplicate,

    /// No extension needed before/after call (high bits are undefined).
    // def NoExt : EnumAttr<"noext", IntersectPreserve, [ParamAttr, RetAttr]>;
    NoExt,

    /// Function does not deallocate memory.
    // def NoFree : EnumAttr<"nofree", IntersectAnd, [FnAttr, ParamAttr]>;
    NoFree,

    /// Argument is dead if the call unwinds.
    // def DeadOnUnwind : EnumAttr<"dead_on_unwind", IntersectAnd, [ParamAttr]>;
    DeadOnUnwind,

    /// Argument is dead upon function return.
    // def DeadOnReturn : EnumAttr<"dead_on_return", IntersectAnd, [ParamAttr]>;
    DeadOnReturn,

    /// Disable implicit floating point insts.
    // def NoImplicitFloat : EnumAttr<"noimplicitfloat", IntersectPreserve, [FnAttr]>;
    NoImplicitFloat,

    /// inline=never.
    // def NoInline : EnumAttr<"noinline", IntersectPreserve, [FnAttr]>;
    NoInline,

    /// Function is called early and/or often, so lazy binding isn't worthwhile.
    // def NonLazyBind : EnumAttr<"nonlazybind", IntersectPreserve, [FnAttr]>;
    NonLazyBind,

    /// Disable merging for specified functions or call sites.
    // def NoMerge : EnumAttr<"nomerge", IntersectPreserve, [FnAttr]>;
    NoMerge,

    /// Pointer is known to be not null.
    // def NonNull : EnumAttr<"nonnull", IntersectAnd, [ParamAttr, RetAttr]>;
    NonNull,

    /// The function does not recurse.
    // def NoRecurse : EnumAttr<"norecurse", IntersectAnd, [FnAttr]>;
    NoRecurse,

    /// Disable redzone.
    // def NoRedZone : EnumAttr<"noredzone", IntersectPreserve, [FnAttr]>;
    NoRedZone,

    /// Mark the function as not returning.
    // def NoReturn : EnumAttr<"noreturn", IntersectAnd, [FnAttr]>;
    NoReturn,

    /// Function does not synchronize.
    // def NoSync : EnumAttr<"nosync", IntersectAnd, [FnAttr]>;
    NoSync,

    /// Disable Indirect Branch Tracking.
    // def NoCfCheck : EnumAttr<"nocf_check", IntersectPreserve, [FnAttr]>;
    NoCfCheck,

    /// Function should not be instrumented.
    // def NoProfile : EnumAttr<"noprofile", IntersectPreserve, [FnAttr]>;
    NoProfile,

    /// This function should not be instrumented but it is ok to inline profiled
    /// functions into it.
    // def SkipProfile : EnumAttr<"skipprofile", IntersectPreserve, [FnAttr]>;
    SkipProfile,

    /// Function doesn't unwind stack.
    // def NoUnwind : EnumAttr<"nounwind", IntersectAnd, [FnAttr]>;
    NoUnwind,

    /// No SanitizeBounds instrumentation.
    // def NoSanitizeBounds : EnumAttr<"nosanitize_bounds", IntersectPreserve, [FnAttr]>;
    NoSanitizeBounds,

    /// No SanitizeCoverage instrumentation.
    // def NoSanitizeCoverage : EnumAttr<"nosanitize_coverage", IntersectPreserve, [FnAttr]>;
    NoSanitizeCoverage,

    /// Null pointer in address space zero is valid.
    // def NullPointerIsValid : EnumAttr<"null_pointer_is_valid", IntersectPreserve, [FnAttr]>;
    NullPointerIsValid,

    /// Select optimizations that give decent debug info.
    // def OptimizeForDebugging : EnumAttr<"optdebug", IntersectPreserve, [FnAttr]>;
    OptimizeForDebugging,

    /// Select optimizations for best fuzzing signal.
    // def OptForFuzzing : EnumAttr<"optforfuzzing", IntersectPreserve, [FnAttr]>;
    OptForFuzzing,

    /// opt_size.
    // def OptimizeForSize : EnumAttr<"optsize", IntersectPreserve, [FnAttr]>;
    OptimizeForSize,

    /// Function must not be optimized.
    // def OptimizeNone : EnumAttr<"optnone", IntersectPreserve, [FnAttr]>;
    OptimizeNone,

    /// Function does not access memory.
    // def ReadNone : EnumAttr<"readnone", IntersectAnd, [ParamAttr]>;
    ReadNone,

    /// Function only reads from memory.
    // def ReadOnly : EnumAttr<"readonly", IntersectAnd, [ParamAttr]>;
    ReadOnly,

    /// Return value is always equal to this argument.
    // def Returned : EnumAttr<"returned", IntersectAnd, [ParamAttr]>;
    Returned,

    /// Parameter is required to be a trivial constant.
    // def ImmArg : EnumAttr<"immarg", IntersectPreserve, [ParamAttr]>;
    ImmArg,

    /// Function can return twice.
    // def ReturnsTwice : EnumAttr<"returns_twice", IntersectPreserve, [FnAttr]>;
    ReturnsTwice,

    /// Safe Stack protection.
    // def SafeStack : EnumAttr<"safestack", IntersectPreserve, [FnAttr]>;
    SafeStack,

    /// Shadow Call Stack protection.
    // def ShadowCallStack : EnumAttr<"shadowcallstack", IntersectPreserve, [FnAttr]>;
    ShadowCallStack,

    /// Sign extended before/after call.
    // def SExt : EnumAttr<"signext", IntersectPreserve, [ParamAttr, RetAttr]>;
    SExt,

    /// Function can be speculated.
    // def Speculatable : EnumAttr<"speculatable", IntersectAnd, [FnAttr]>;
    Speculatable,

    /// Stack protection.
    // def StackProtect : EnumAttr<"ssp", IntersectPreserve, [FnAttr]>;
    StackProtect,

    /// Stack protection required.
    // def StackProtectReq : EnumAttr<"sspreq", IntersectPreserve, [FnAttr]>;
    StackProtectReq,

    /// Strong Stack protection.
    // def StackProtectStrong : EnumAttr<"sspstrong", IntersectPreserve, [FnAttr]>;
    StackProtectStrong,

    /// Function was called in a scope requiring strict floating point semantics.
    // def StrictFP : EnumAttr<"strictfp", IntersectPreserve, [FnAttr]>;
    StrictFP,

    /// AddressSanitizer is on.
    // def SanitizeAddress : EnumAttr<"sanitize_address", IntersectPreserve, [FnAttr]>;
    SanitizeAddress,

    /// ThreadSanitizer is on.
    // def SanitizeThread : EnumAttr<"sanitize_thread", IntersectPreserve, [FnAttr]>;
    SanitizeThread,

    /// TypeSanitizer is on.
    // def SanitizeType : EnumAttr<"sanitize_type", IntersectPreserve, [FnAttr]>;
    SanitizeType,

    /// MemorySanitizer is on.
    // def SanitizeMemory : EnumAttr<"sanitize_memory", IntersectPreserve, [FnAttr]>;
    SanitizeMemory,

    /// HWAddressSanitizer is on.
    // def SanitizeHWAddress : EnumAttr<"sanitize_hwaddress", IntersectPreserve, [FnAttr]>;
    SanitizeHWAddress,

    /// MemTagSanitizer is on.
    // def SanitizeMemTag : EnumAttr<"sanitize_memtag", IntersectPreserve, [FnAttr]>;
    SanitizeMemTag,

    /// NumericalStabilitySanitizer is on.
    // def SanitizeNumericalStability : EnumAttr<"sanitize_numerical_stability", IntersectPreserve, [FnAttr]>;
    SanitizeNumericalStability,

    /// RealtimeSanitizer is on.
    // def SanitizeRealtime : EnumAttr<"sanitize_realtime", IntersectPreserve, [FnAttr]>;
    SanitizeRealtime,

    /// RealtimeSanitizer should error if a real-time unsafe function is invoked
    /// during a real-time sanitized function (see `sanitize_realtime`).
    // def SanitizeRealtimeBlocking : EnumAttr<"sanitize_realtime_blocking", IntersectPreserve, [FnAttr]>;
    SanitizeRealtimeBlocking,

    /// Speculative Load Hardening is enabled.
    ///
    /// Note that this uses the default compatibility (always compatible during
    /// inlining) and a conservative merge strategy where inlining an attributed
    /// body will add the attribute to the caller. This ensures that code carrying
    /// this attribute will always be lowered with hardening enabled.
    // def SpeculativeLoadHardening : EnumAttr<"speculative_load_hardening", IntersectPreserve, [FnAttr]>;
    SpeculativeLoadHardening,

    /// Argument is swift error.
    // def SwiftError : EnumAttr<"swifterror", IntersectPreserve, [ParamAttr]>;
    SwiftError,

    /// Argument is swift self/context.
    // def SwiftSelf : EnumAttr<"swiftself", IntersectPreserve, [ParamAttr]>;
    SwiftSelf,

    /// Argument is swift async context.
    // def SwiftAsync : EnumAttr<"swiftasync", IntersectPreserve, [ParamAttr]>;
    SwiftAsync,

    /// Function always comes back to callsite.
    // def WillReturn : EnumAttr<"willreturn", IntersectAnd, [FnAttr]>;
    WillReturn,

    /// Pointer argument is writable.
    // def Writable : EnumAttr<"writable", IntersectAnd, [ParamAttr]>;
    Writable,

    /// Function only writes to memory.
    // def WriteOnly : EnumAttr<"writeonly", IntersectAnd, [ParamAttr]>;
    WriteOnly,

    /// Zero extended before/after call.
    // def ZExt : EnumAttr<"zeroext", IntersectPreserve, [ParamAttr, RetAttr]>;
    ZExt,

    /// Function is required to make Forward Progress.
    // def MustProgress : EnumAttr<"mustprogress", IntersectAnd, [FnAttr]>;
    MustProgress,

    /// Function is a presplit coroutine.
    // def PresplitCoroutine : EnumAttr<"presplitcoroutine", IntersectPreserve, [FnAttr]>;
    PresplitCoroutine,

    /// The coroutine would only be destroyed when it is complete.
    // def CoroDestroyOnlyWhenComplete : EnumAttr<"coro_only_destroy_when_complete", IntersectPreserve, [FnAttr]>;
    CoroDestroyOnlyWhenComplete,

    /// The coroutine call meets the elide requirement. Hint the optimization
    /// pipeline to perform elide on the call or invoke instruction.
    // def CoroElideSafe : EnumAttr<"coro_elide_safe", IntersectPreserve, [FnAttr]>;
    CoroElideSafe,
}

impl AttributeEnumKind {
    pub fn from_raw(raw: u32) -> Result<Self, ParseError> {
        let str_name = Self::get_raw_name(raw);
        Self::from_str(&str_name)
    }

    pub fn get_raw_name(kind: u32) -> String {
        let str_name = unsafe {
            let name_ptr = ffi::amice_attribute_enum_kind_to_str(kind);
            let c_name = CStr::from_ptr(name_ptr);
            let name = c_name.to_str().expect("Invalid attribute name").to_string();
            ffi::amice_free_msg(name_ptr);
            name
        };
        str_name
    }

    pub fn is_param_attribute(self) -> bool {
        matches!(
            self,
            AttributeEnumKind::AllocAlign
                | AttributeEnumKind::AllocatedPointer
                | AttributeEnumKind::Nest
                | AttributeEnumKind::NoFree
                | AttributeEnumKind::DeadOnUnwind
                | AttributeEnumKind::DeadOnReturn
                | AttributeEnumKind::ReadNone
                | AttributeEnumKind::ReadOnly
                | AttributeEnumKind::Returned
                | AttributeEnumKind::ImmArg
                | AttributeEnumKind::SwiftError
                | AttributeEnumKind::SwiftSelf
                | AttributeEnumKind::SwiftAsync
                | AttributeEnumKind::Writable
                | AttributeEnumKind::WriteOnly
                // Param or Ret
                | AttributeEnumKind::NoUndef
                | AttributeEnumKind::InReg
                | AttributeEnumKind::NoAlias
                | AttributeEnumKind::NoExt
                | AttributeEnumKind::NonNull
                | AttributeEnumKind::SExt
                | AttributeEnumKind::ZExt
        )
    }

    pub fn is_return_attribute(self) -> bool {
        matches!(
            self,
            AttributeEnumKind::NoUndef
                | AttributeEnumKind::InReg
                | AttributeEnumKind::NoAlias
                | AttributeEnumKind::NoExt
                | AttributeEnumKind::NonNull
                | AttributeEnumKind::SExt
                | AttributeEnumKind::ZExt
        )
    }

    pub fn is_function_attribute(self) -> bool {
        matches!(
            self,
            AttributeEnumKind::AlwaysInline
                | AttributeEnumKind::Builtin
                | AttributeEnumKind::Cold
                | AttributeEnumKind::Convergent
                | AttributeEnumKind::Hot
                | AttributeEnumKind::DisableSanitizerInstrumentation
                | AttributeEnumKind::FnRetThunkExtern
                | AttributeEnumKind::HybridPatchable
                | AttributeEnumKind::InlineHint
                | AttributeEnumKind::JumpTable
                | AttributeEnumKind::MinSize
                | AttributeEnumKind::Naked
                | AttributeEnumKind::NoBuiltin
                | AttributeEnumKind::NoCallback
                | AttributeEnumKind::NoDivergenceSource
                | AttributeEnumKind::NoDuplicate
                | AttributeEnumKind::NoImplicitFloat
                | AttributeEnumKind::NoInline
                | AttributeEnumKind::NonLazyBind
                | AttributeEnumKind::NoMerge
                | AttributeEnumKind::NoRecurse
                | AttributeEnumKind::NoRedZone
                | AttributeEnumKind::NoReturn
                | AttributeEnumKind::NoSync
                | AttributeEnumKind::NoCfCheck
                | AttributeEnumKind::NoProfile
                | AttributeEnumKind::SkipProfile
                | AttributeEnumKind::NoUnwind
                | AttributeEnumKind::NoSanitizeBounds
                | AttributeEnumKind::NoSanitizeCoverage
                | AttributeEnumKind::NullPointerIsValid
                | AttributeEnumKind::OptimizeForDebugging
                | AttributeEnumKind::OptForFuzzing
                | AttributeEnumKind::OptimizeForSize
                | AttributeEnumKind::OptimizeNone
                | AttributeEnumKind::ReturnsTwice
                | AttributeEnumKind::SafeStack
                | AttributeEnumKind::ShadowCallStack
                | AttributeEnumKind::Speculatable
                | AttributeEnumKind::StackProtect
                | AttributeEnumKind::StackProtectReq
                | AttributeEnumKind::StackProtectStrong
                | AttributeEnumKind::StrictFP
                | AttributeEnumKind::SanitizeAddress
                | AttributeEnumKind::SanitizeThread
                | AttributeEnumKind::SanitizeType
                | AttributeEnumKind::SanitizeMemory
                | AttributeEnumKind::SanitizeHWAddress
                | AttributeEnumKind::SanitizeMemTag
                | AttributeEnumKind::SanitizeNumericalStability
                | AttributeEnumKind::SanitizeRealtime
                | AttributeEnumKind::SanitizeRealtimeBlocking
                | AttributeEnumKind::SpeculativeLoadHardening
                | AttributeEnumKind::WillReturn
                | AttributeEnumKind::MustProgress
                | AttributeEnumKind::PresplitCoroutine
                | AttributeEnumKind::CoroDestroyOnlyWhenComplete
                | AttributeEnumKind::CoroElideSafe
                // Both Fn and Param
                | AttributeEnumKind::NoFree
        )
    }

    pub fn all() -> Vec<Self> {
        Self::iter().collect()
    }
}
