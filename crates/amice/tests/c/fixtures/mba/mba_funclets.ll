target triple = "x86_64-pc-windows-msvc"
declare void @may_throw()
declare i32 @__CxxFrameHandler3(...)

define i32 @funclet(i32 %x, i32 %y) personality ptr @__CxxFrameHandler3 {
entry:
  %a = add i32 %x, %y
  invoke void @may_throw() to label %normal unwind label %dispatch
normal:
  br label %exit
dispatch:
  %d = phi i32 [ %a, %entry ]
  %cs = catchswitch within none [label %handler] unwind to caller
handler:
  %p = phi i32 [ %d, %dispatch ]
  %cp = catchpad within %cs [ptr null, i32 64, ptr null]
  %b = sub i32 %p, %y
  catchret from %cp to label %exit
exit:
  %result = phi i32 [ %a, %normal ], [ %b, %handler ]
  ret i32 %result
}

define void @cleanup(i32 %x, i32 %y, ptr %out) personality ptr @__CxxFrameHandler3 {
entry:
  %a = add i32 %x, %y
  invoke void @may_throw() to label %normal unwind label %cleanup
normal:
  ret void
cleanup:
  %p = phi i32 [ %a, %entry ]
  %cl = cleanuppad within none []
  %b = sub i32 %p, %y
  store i32 %b, ptr %out
  cleanupret from %cl unwind to caller
}
