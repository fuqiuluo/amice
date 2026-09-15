; Hand-written IR keeps this ABI-attribute test independent of the host target's
; C ABI. AArch64, for example, does not spell narrow integer extension using
; signext/zeroext in the IR emitted by Clang.

define internal signext i8 @abi_narrow(i8 signext %first, i8 zeroext %second, i8 signext %third) noinline {
entry:
  %sum = add i8 %first, %second
  %result = sub i8 %sum, %third
  ret i8 %result
}

define i32 @main(i32 %argc, ptr %argv) {
entry:
  %first = trunc i32 %argc to i8
  %result = call signext i8 @abi_narrow(i8 signext %first, i8 zeroext 7, i8 signext 3)
  %exit_code = zext i8 %result to i32
  ret i32 %exit_code
}
