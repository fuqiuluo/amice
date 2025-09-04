// translate from https://github.com/HikariObfuscator/Core/blob/077f079/AntiClassDump.cpp

use crate::config::{AntiClassDumpConfig, Config};
use crate::pass_registry::{AmicePass, AmicePassFlag};
use amice_llvm::inkwell2::{BasicBlockExt, FunctionExt, LLVMValueRefExt};
use amice_macro::amice;
use anyhow::anyhow;
use llvm_plugin::PreservedAnalyses;
use llvm_plugin::inkwell::basic_block::BasicBlock;
use llvm_plugin::inkwell::llvm_sys::core::{
    LLVMCountStructElementTypes, LLVMGetAggregateElement, LLVMGetAsString, LLVMGetInitializer, LLVMGetNumOperands,
    LLVMGetOperand, LLVMGetStructElementTypes, LLVMGetStructName, LLVMIsNull, LLVMPointerType, LLVMTypeOf,
};
use llvm_plugin::inkwell::llvm_sys::prelude::LLVMValueRef;
use llvm_plugin::inkwell::module::{Linkage, Module};
use llvm_plugin::inkwell::types::AsTypeRef;
use llvm_plugin::inkwell::values::{
    ArrayValue, AsValueRef, BasicValue, BasicValueEnum, GlobalValue, InstructionValue, StructValue,
};
use std::collections::{HashMap, VecDeque};
use std::ffi::{CStr, CString, c_uint};
use std::slice;

const INFO_KEY_IVARLIST: &str = "IVARLIST";
const INFO_KEY_PROPLIST: &str = "PROPLIST";
const INFO_KEY_METHODLIST: &str = "METHODLIST";

#[amice(
    priority = 1500,
    name = "AntiClassDump",
    flag = AmicePassFlag::PipelineStart | AmicePassFlag::ModuleLevel,
    config = AntiClassDumpConfig,
)]
#[derive(Default)]
pub struct AntiClassDump {}

impl AmicePass for AntiClassDump {
    fn init(&mut self, cfg: &Config, _flag: AmicePassFlag) {
        self.default_config = cfg.anti_class_dump.clone();
    }

    fn do_pass(&self, module: &mut Module<'_>) -> anyhow::Result<PreservedAnalyses> {
        if !self.default_config.enable {
            return Ok(PreservedAnalyses::All);
        }

        let triple = module.get_triple();
        if !triple.as_str().to_string_lossy().contains("apple") {
            warn!("current target triple {triple} is not an Apple platform, skipping");
            return Ok(PreservedAnalyses::All);
        }

        let Some(objc_label_class) = module.get_global("OBJC_LABEL_CLASS_$") else {
            warn!("OBJC_LABEL_CLASS_$ not found, skipping");
            return Ok(PreservedAnalyses::All);
        };

        let Some(objc_label_class_cds) = objc_label_class.get_initializer() else {
            warn!("OBJC_LABEL_CLASS_$ is not a constant, skipping");
            return Ok(PreservedAnalyses::All);
        };

        let objc_label_class_cds = match objc_label_class_cds {
            BasicValueEnum::ArrayValue(arr) => arr,
            _ => {
                warn!("OBJC_LABEL_CLASS_$ is not an array, skipping");
                return Ok(PreservedAnalyses::All);
            },
        };

        let Some(objc_label_class_cds) = objc_label_class_cds.as_instruction() else {
            warn!("OBJC_LABEL_CLASS_$ is not an instruction, skipping");
            return Ok(PreservedAnalyses::All);
        };

        if let Err(e) = unsafe { do_handle(&self.default_config, module, objc_label_class_cds) } {
            error!("failed to handle anti class dump: {e}");
            return Ok(PreservedAnalyses::All);
        }

        Ok(PreservedAnalyses::None)
    }
}

unsafe fn do_handle(
    cfg: &AntiClassDumpConfig,
    module: &mut Module<'_>,
    objc_label_class_cds: InstructionValue,
) -> anyhow::Result<()> {
    let mut ready_cls = Vec::<String>::new();
    let mut tmp_cls = VecDeque::<String>::new();
    let mut dependency = HashMap::<String /*class*/, String /*super class*/>::new();
    let mut gv_mapping = HashMap::<String /*class*/, GlobalValue>::new();

    let objc_label_class_cds = objc_label_class_cds.as_value_ref();
    for i in 0..LLVMGetNumOperands(objc_label_class_cds) as u32 {
        let cls_expr = LLVMGetOperand(objc_label_class_cds, i as c_uint);
        let cegv = LLVMGetOperand(cls_expr, 0);
        let cls_cs = LLVMGetInitializer(cegv);

        let super_cls_gv = if LLVMGetOperand(cls_cs, 1).is_null() {
            std::ptr::null_mut()
        } else {
            LLVMGetOperand(cls_cs, 1)
        } as LLVMValueRef;

        let cegv = cegv.into_global_value();
        let cls_name = cegv.get_name().to_string_lossy().replace("OBJC_CLASS_$_", "");

        let super_cls_name = if !super_cls_gv.is_null() {
            let super_cls_gv = super_cls_gv.into_global_value();
            super_cls_gv.get_name().to_string_lossy().replace("OBJC_CLASS_$_", "")
        } else {
            "".to_string()
        };

        dependency.insert(cls_name.clone(), super_cls_name.clone());
        gv_mapping.insert(cls_name.clone(), cegv);
        if super_cls_name.is_empty() /*NULL Super Class*/
            || (!super_cls_gv.is_null() && super_cls_gv.into_global_value().get_initializer().is_none() /*External Super Class*/)
        {
            ready_cls.push(cls_name);
        } else {
            tmp_cls.push_back(cls_name);
        }
    }

    // Sort Initialize Sequence Based On Dependency
    while let Some(cls) = tmp_cls.pop_front() {
        let super_class_name = dependency.get(&cls);
        if let Some(super_class_name) = super_class_name
            && !super_class_name.is_empty()
            && !ready_cls.contains(super_class_name)
        {
            // SuperClass is unintialized non-null class.Push back and waiting until
            // baseclass is allocated
            tmp_cls.push_back(cls);
        } else {
            ready_cls.push(cls);
        }
    }

    for cls_name in ready_cls {
        handle_class(cfg, module, gv_mapping[&cls_name])?;
    }

    Ok(())
}

unsafe fn handle_class<'a>(
    cfg: &AntiClassDumpConfig,
    module: &mut Module<'a>,
    global_value: GlobalValue<'a>,
) -> anyhow::Result<()> {
    if global_value.get_initializer().is_none() {
        return Err(anyhow!("class {:?} is not initialized", global_value.get_name()));
    }

    let cs = global_value.get_initializer().unwrap().as_value_ref();
    let class_name = global_value.get_name().to_string_lossy().replace("OBJC_CLASS_$_", "");
    let super_class_name = LLVMGetOperand(cs, 1)
        .into_global_value()
        .get_name()
        .to_string_lossy()
        .replace("OBJC_CLASS_$_", "");

    // Let's extract stuffs
    // struct _class_t {
    //   struct _class_t *isa;
    //   struct _class_t * const superclass;
    //   void *cache;
    //   IMP *vtable;
    //   struct class_ro_t *ro;
    // }
    let meta_class_gv = LLVMGetOperand(cs, 0).into_global_value();
    let class_ro = LLVMGetOperand(cs, 4).into_global_value();
    if meta_class_gv.get_initializer().is_none() {
        return Err(anyhow!("meta class {:?} is not initialized", meta_class_gv.get_name()));
    }
    let meta_class_gv_initializer = meta_class_gv.get_initializer().unwrap().as_value_ref();
    let meta_class_ro = LLVMGetOperand(
        meta_class_gv_initializer,
        LLVMGetNumOperands(meta_class_gv_initializer) as c_uint - 1,
    )
    .into_global_value();

    let ctx = module.get_context();
    let builder = ctx.create_builder();

    let mut entry_block = None::<BasicBlock>;
    let info = split_class_ro_t(module, meta_class_ro.get_initializer().unwrap().as_value_ref())?;
    if info.contains_key(INFO_KEY_METHODLIST) {
        let method_list = info[INFO_KEY_METHODLIST];
        for i in 0..LLVMGetNumOperands(method_list) {
            let method_struct = LLVMGetOperand(method_list, i as c_uint);
            // methodStruct has type %struct._objc_method = type { i8*, i8*, i8* }
            // which contains {GEP(NAME),GEP(TYPE),BitCast(IMP)}
            // Let's extract these info now
            // methodStruct->getOperand(0)->getOperand(0) is SELName
            let sel_name_gv = LLVMGetOperand(LLVMGetOperand(method_struct, 0), 0).into_global_value();
            let arr = match sel_name_gv
                .get_initializer()
                .ok_or_else(|| anyhow!("SELName {:?} is not initialized", sel_name_gv.get_name()))?
            {
                // C-like strings
                BasicValueEnum::ArrayValue(arr) => arr,
                _ => {
                    return Err(anyhow!(
                        "SELName {:?} is not an array, skipping",
                        sel_name_gv.get_name()
                    ));
                },
            };
            if !arr.is_const_string() || arr.get_type().is_empty() {
                return Err(anyhow!(
                    "SELName {:?} is not a valid string, skipping",
                    sel_name_gv.get_name()
                ));
            }
            let sel_name = array_as_const_string(&arr)
                .ok_or_else(|| anyhow!("SELName {:?} is not a valid string, skipping", sel_name_gv.get_name()))?;
            if (sel_name == "initialize" && cfg.use_initialize) || (sel_name == "load" && !cfg.use_initialize) {
                let imp_func = LLVMGetOperand(LLVMGetOperand(method_struct, 2), 0)
                    .into_function_value()
                    .ok_or_else(|| anyhow!("SELName {:?} is not a valid function", method_struct))?;
                entry_block = imp_func.get_entry_block();
            }
        }
    } else {
        warn!("class {:?} has no method list", class_name);
    }

    let mut need_terminator = false;
    if entry_block.is_none() {
        need_terminator = true;
        // We failed to find existing +initializer,create new one
        debug!("creating initializer");
        let initializer_type = ctx.void_type().fn_type(&[], false);
        let initializer = module.add_function("anti_dump_initializer", initializer_type, Some(Linkage::Private));
        entry_block = ctx.append_basic_block(initializer, "").into();
    }

    let entry_block = entry_block.ok_or_else(|| anyhow!("no entry block found"))?;
    if need_terminator {
        builder.position_at_end(entry_block);
        builder.build_return(None)?;
    }

    builder.position_before(&entry_block.get_first_insertion_pt());
    let objc_get_class = module.get_function("objc_getClass");
    // Type *Int8PtrTy = Type::getInt8PtrTy(M->getContext());
    // End of ObjC API Definitions
    let class_name_gv = builder.build_global_string_ptr(&class_name, "")?;
    // Now Scan For Props and Ivars in OBJC_CLASS_RO AND OBJC_METACLASS_RO
    // Note that class_ro_t's structure is different for 32 and 64bit runtime
    let class = builder.build_call(objc_get_class.unwrap(), &[class_name_gv.as_pointer_value().into()], "")?;
    // Add Method
    let meta_class_cs = class_ro.get_initializer().unwrap();
    let class_cs = meta_class_ro.get_initializer().unwrap();

    if LLVMIsNull(LLVMGetAggregateElement(meta_class_cs.as_value_ref(), 5)) {
        warn!("Handling Instance Methods For Class: {}", class_name);
    }

    Ok(())
}

unsafe fn split_class_ro_t(
    module: &mut Module,
    class_ro: LLVMValueRef,
) -> anyhow::Result<HashMap<String, LLVMValueRef>> {
    let mut info = HashMap::new();
    let objc_method_list_t_type = module
        .get_struct_type("struct.__method_list_t")
        .ok_or_else(|| anyhow!("struct.__method_list_t not found"))?;
    let ivar_list_t_type = module
        .get_struct_type("struct._ivar_list_t")
        .ok_or_else(|| anyhow!("struct._ivar_list_t not found"))?;
    let property_list_t_type = module
        .get_struct_type("struct._prop_list_t")
        .ok_or_else(|| anyhow!("struct._prop_list_t not found"))?;

    let class_ro_type = LLVMTypeOf(class_ro);
    for i in 0..LLVMCountStructElementTypes(class_ro_type) {
        let element = LLVMGetAggregateElement(class_ro, i);
        if element.is_null() {
            continue;
        }
        let element_type = LLVMTypeOf(element);
        if element_type == LLVMPointerType(ivar_list_t_type.as_type_ref(), 0) {
            info.insert(INFO_KEY_IVARLIST.to_string(), element);
        } else if element_type == LLVMPointerType(property_list_t_type.as_type_ref(), 0) {
            info.insert(INFO_KEY_PROPLIST.to_string(), element);
        } else if element_type == LLVMPointerType(objc_method_list_t_type.as_type_ref(), 0) {
            // Insert Methods
            let method_list_ce = element;
            // Note:methodListCE is also a BitCastConstantExpr
            let method_list_gv = LLVMGetOperand(method_list_ce, 0).into_global_value();
            // Now BitCast is stripped out.
            if method_list_gv.get_initializer().is_none() {
                return Err(anyhow!(
                    "method list {:?} is not initialized",
                    method_list_gv.get_name()
                ));
            }
            let method_list_struct = method_list_gv.get_initializer().unwrap().as_value_ref();
            // Extracting %struct._objc_method array from %struct.__method_list_t =
            // type { i32, i32, [0 x %struct._objc_method] }
            info.insert(INFO_KEY_METHODLIST.to_string(), LLVMGetOperand(method_list_struct, 2));
        }
    }

    Ok(info)
}

pub(crate) fn array_as_const_string(arr: &ArrayValue) -> Option<String> {
    let mut len = 0;
    let ptr = unsafe { LLVMGetAsString(arr.as_value_ref(), &mut len) };
    if ptr.is_null() {
        None
    } else {
        unsafe { Some(String::from_utf8_lossy(slice::from_raw_parts(ptr.cast(), len)).to_string()) }
    }
}
