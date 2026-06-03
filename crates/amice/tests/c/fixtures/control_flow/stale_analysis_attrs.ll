define i32 @pure_branch(i32 %x) nounwind willreturn readnone {
entry:
  %c = icmp sgt i32 %x, 0
  br i1 %c, label %pos, label %neg

pos:
  ret i32 1

neg:
  ret i32 2
}
