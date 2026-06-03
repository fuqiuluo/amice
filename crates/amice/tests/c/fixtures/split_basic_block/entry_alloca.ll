define i32 @f(i32 %x) {
entry:
  %slot = alloca i32, align 4
  store i32 %x, ptr %slot, align 4
  %a = load i32, ptr %slot, align 4
  %b = add i32 %a, 1
  %c = mul i32 %b, 3
  %d = sub i32 %c, 4
  ret i32 %d
}
