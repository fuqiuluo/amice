define i32 @f(i1 %c) {
entry:
  br i1 %c, label %t, label %e

t:
  ret i32 1

e:
  ret i32 0
}

define i32 @main() {
entry:
  %v = call i32 @f(i1 false)
  ret i32 %v
}
