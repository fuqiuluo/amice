# Amice 

# Runtime environment variables

| 变量名               | 说明                                                                                                                             | 默认值  |
|-------------------|--------------------------------------------------------------------------------------------------------------------------------|------|
| AMICE_STR_DECRYPT | 控制字符串的解密时机：<br/>• `global` —— 程序启动时在全局初始化阶段一次性解密所有受保护字符串；<br/>• `lazy` —— 在每个字符串首次被使用前按需解密（随后可缓存）。 <br/>  备注：解密在栈上的字符串不支持这个配置！ | lazy |


# Thanks

- [jamesmth/llvm-plugin-rs](https://github.com/jamesmth/llvm-plugin-rs/tree/feat/llvm-20#)
- [stevefan1999-personal/llvm-plugin-rs](https://github.com/stevefan1999-personal/llvm-plugin-rs)
- [llvm PassManager的变更及动态注册Pass的加载过程](https://bbs.kanxue.com/thread-272801.htm)
- [SsagePass](https://github.com/SsageParuders/SsagePass)