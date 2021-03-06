use crate::error;
use crate::ir::*;
use llvm_sys::*;
use std::ffi::CStr;
use std::mem::MaybeUninit;
use std::ops::Deref;
use std::ptr;

macro_rules! cstr {
    ($s:expr) => {{
        concat!($s, "\0").as_ptr() as *const i8
    }};
}

pub unsafe fn build(module: &Module2) -> (LLVMTargetMachineRef, LLVMModuleRef) {
    LLVMInitializeX86TargetInfo();
    LLVMInitializeX86Target();
    LLVMInitializeX86TargetMC();
    LLVMInitializeX86AsmPrinter();

    let triple = LLVMGetDefaultTargetTriple();
    let mut target = MaybeUninit::uninit().assume_init();
    let mut err = MaybeUninit::uninit().assume_init();
    if LLVMGetTargetFromTriple(triple, &mut target, &mut err) != 0 {
        let err = CStr::from_ptr(err);
        println!("error getting llvm target: {:?}", err);
        error();
    }

    let cpu = cstr!("generic");
    let features = cstr!("");
    let machine = LLVMCreateTargetMachine(
        target,
        triple,
        cpu,
        features,
        LLVMCodeGenOptLevel_LLVMCodeGenLevelNone,
        LLVMRelocMode_LLVMRelocDefault,
        LLVMCodeModel_LLVMCodeModelDefault,
    );
    let layout = LLVMCreateTargetDataLayout(machine);

    let llmodule = LLVMModuleCreateWithName(cstr!("a"));
    LLVMSetModuleDataLayout(llmodule, layout);
    LLVMSetTarget(llmodule, triple);

    let b = LLVMCreateBuilder();
    let type_bld = &TypeBuilder::new(layout, &module.types);
    let llconsts = &build_consts(type_bld, &module.consts);

    let mut llfuncs = vec![];
    for func_decl in &module.func_decls {
        let lltype = type_bld.func_type(&func_decl.ty);
        let mut name = func_decl.name.deref().to_string();
        name.push('\0');
        let mut link_name = name.as_ptr() as *const i8;
        if cfg!(target_os = "macos") && name == "readdir\0" {
            link_name = "readdir$INODE64\0".as_ptr() as *const i8;
        }
        let llfunc = LLVMAddFunction(llmodule, link_name, lltype);
        llfuncs.push(llfunc);
    }
    let llfuncs = &llfuncs;

    for func_body in &module.func_bodys {
        let func_decl = &module.func_decls[func_body.id];
        build_func_body(b, type_bld, llfuncs, llconsts, func_decl, func_body);
    }

    (machine, llmodule)
}

pub unsafe fn verify(llmodule: LLVMModuleRef) {
    let mut msg = ptr::null_mut();
    LLVMVerifyModule(
        llmodule,
        LLVMVerifierFailureAction_LLVMAbortProcessAction,
        &mut msg,
    );
}

pub unsafe fn emit_object(machine: LLVMTargetMachineRef, llmodule: LLVMModuleRef) {
    let mut msg = ptr::null_mut();
    if LLVMTargetMachineEmitToFile(
        machine,
        llmodule,
        cstr!("a.o") as *mut i8,
        LLVMCodeGenFileType_LLVMObjectFile,
        &mut msg,
    ) != 0
    {
        let msg = CStr::from_ptr(msg);
        println!("error emitting object file: {:?}", msg);
        error();
    }
}

unsafe fn build_consts(types: &TypeBuilder, consts: &[Const]) -> Vec<LLVMValueRef> {
    let mut b = ConstBuilder {
        consts: vec![None; consts.len()],
        types: types,
    };
    for (id, c) in consts.iter().enumerate() {
        let v = b.build(c);
        b.consts[id] = Some(v);
    }
    let mut consts = vec![];
    for c in b.consts {
        consts.push(c.unwrap());
    }
    consts
}

struct ConstBuilder<'a> {
    consts: Vec<Option<LLVMValueRef>>,
    types: &'a TypeBuilder<'a>,
}

impl<'a> ConstBuilder<'a> {
    unsafe fn build(&mut self, c: &Const) -> LLVMValueRef {
        match &c.expr.kind {
            ExprKind::Integer(s) => {
                let lltype = self.types.lltype(c.expr.ty);
                let ptr = s.as_ptr() as *const i8;
                let len = s.len() as u32;
                let radix = 10;
                LLVMConstIntOfStringAndSize(lltype, ptr, len, radix)
            }
            _ => panic!(),
        }
    }
}

struct TypeBuilder<'a> {
    layout: LLVMTargetDataRef,
    lltypes: Vec<LLVMTypeRef>,
    types: &'a [Type],
}

impl<'a> TypeBuilder<'a> {
    unsafe fn new(layout: LLVMTargetDataRef, types: &'a [Type]) -> Self {
        let mut b = TypeBuilder {
            layout: layout,
            lltypes: vec![],
            types: types,
        };
        for type_id in 0..types.len() {
            let lltype = b.build_type(type_id);
            b.lltypes.push(lltype);
        }
        for (id, ty) in types.iter().enumerate() {
            if let Type::Struct(sty) = ty {
                b.set_struct_body(id, sty);
            }
            if let Type::Enum(ety) = ty {
                b.set_enum_body(id, ety);
            }
        }
        b
    }

    unsafe fn build_type(&self, ty: TypeId) -> LLVMTypeRef {
        if let Some(&lltype) = self.lltypes.get(ty) {
            return lltype;
        }
        match self.irtype(ty) {
            Type::Bool => LLVMInt1Type(),
            Type::I8 => LLVMInt8Type(),
            Type::I16 => LLVMInt16Type(),
            Type::I32 => LLVMInt32Type(),
            Type::I64 => LLVMInt64Type(),
            Type::F32 => LLVMFloatType(),
            Type::F64 => LLVMDoubleType(),
            Type::Pointer(ty) => {
                let lltype = self.build_type(*ty);
                LLVMPointerType(lltype, 0)
            }
            Type::Func(ty) => self.func_type(ty),
            Type::Unit => LLVMVoidType(),
            Type::Struct(ty) => {
                let mut name = ty.name.to_string();
                name.push('\0');
                LLVMStructCreateNamed(LLVMGetGlobalContext(), name.as_ptr() as *const i8)
            }
            Type::Array(elem_ty, n) => {
                let elem_ty = self.build_type(*elem_ty);
                LLVMArrayType(elem_ty, *n)
            }
            Type::Tuple(elem_tys) => {
                let mut ll_elem_tys = vec![];
                for &elem_ty in elem_tys {
                    let elem_ty = self.build_type(elem_ty);
                    ll_elem_tys.push(elem_ty);
                }
                LLVMStructType(ll_elem_tys.as_mut_ptr(), ll_elem_tys.len() as u32, 0)
            }
            Type::Enum(ety) => {
                let mut name = ety.name.to_string();
                name.push('\0');
                let name = name.as_ptr() as *const i8;
                LLVMStructCreateNamed(LLVMGetGlobalContext(), name)
            }
        }
    }

    unsafe fn set_struct_body(&self, id: TypeId, sty: &StructType) {
        let lltype = self.lltype(id);
        let mut elem_types = vec![];
        for &(_, ty) in &sty.fields {
            let ty = self.lltype(ty);
            elem_types.push(ty);
        }
        LLVMStructSetBody(lltype, elem_types.as_mut_ptr(), elem_types.len() as u32, 0);
    }

    unsafe fn set_enum_body(&self, id: TypeId, ety: &EnumType) {
        let enum_struct = self.lltype(id);
        let tag_type = LLVMInt8Type();

        // Create struct types for each variant.
        let mut largest: Option<(u64, LLVMTypeRef)> = None;
        for variant in &ety.variants {
            let mut args = vec![];
            for &arg in &variant.args {
                let ty = self.build_type(arg);
                args.push(ty);
            }
            let p = args.as_mut_ptr();
            let n = args.len() as u32;
            let ty = LLVMStructType(p, n, 0);
            let size = LLVMStoreSizeOfType(self.layout, ty);

            largest = match largest {
                Some((n, other)) if n >= size => Some((n, other)),
                _ => Some((size, ty)),
            };
        }
        let mut fields = match largest {
            Some((_, ty)) => vec![ty, tag_type],
            _ => vec![tag_type],
        };
        let p = fields.as_mut_ptr();
        let n = fields.len() as u32;
        LLVMStructSetBody(enum_struct, p, n, 0);
    }

    fn irtype(&self, ty: TypeId) -> &'a Type {
        &self.types[ty]
    }

    fn lltype(&self, ty: TypeId) -> LLVMTypeRef {
        self.lltypes[ty]
    }

    unsafe fn func_type(&self, func: &FuncType) -> LLVMTypeRef {
        let mut params = vec![];
        for &ty in &func.params {
            let ty = match self.irtype(ty).kind() {
                TypeKind::Aggregate => {
                    let sty = self.lltype(ty);
                    LLVMPointerType(sty, 0)
                }
                TypeKind::Unit => continue,
                TypeKind::Scalar => self.lltype(ty),
            };
            params.push(ty);
        }

        let ret = match self.irtype(func.ret).kind() {
            TypeKind::Aggregate => {
                let ret = self.lltype(func.ret);
                let sret = LLVMPointerType(ret, 0);
                params.push(sret);
                LLVMVoidType()
            }
            TypeKind::Unit => LLVMVoidType(),
            TypeKind::Scalar => self.lltype(func.ret),
        };
        let var_args = if func.var_args { 1 } else { 0 };

        LLVMFunctionType(ret, params.as_mut_ptr(), params.len() as u32, var_args)
    }
}

unsafe fn build_func_body(
    b: LLVMBuilderRef,
    type_bld: &TypeBuilder,
    llfuncs: &[LLVMValueRef],
    llconsts: &[LLVMValueRef],
    func: &FuncDecl,
    body: &FuncBody,
) {
    let llfunc = llfuncs[body.id];
    let entry = LLVMAppendBasicBlock(llfunc, cstr!("entry"));
    LLVMPositionBuilderAtEnd(b, entry);

    let sret = match type_bld.irtype(func.ty.ret).kind() {
        TypeKind::Aggregate => Some(LLVMGetLastParam(llfunc)),
        TypeKind::Unit => None,
        TypeKind::Scalar => None,
    };

    let mut locals = vec![];
    for &ty in &body.locals {
        let lltype = type_bld.lltype(ty);
        let p = LLVMBuildAlloca(b, lltype, cstr!(""));
        locals.push(p);
    }
    let locals = &locals;

    let mut b = StmtBuilder {
        bld: b,
        tybld: type_bld,

        llfuncs: llfuncs,
        llconsts: llconsts,
        llfunc: llfunc,
        locals: locals,
        sret: sret,

        break_dest: vec![],
        continue_dest: vec![],
        block: entry,
    };
    b.build_block(&body.body);

    let term = LLVMGetBasicBlockTerminator(b.block);
    if term.is_null() {
        LLVMBuildRetVoid(b.bld);
    }
}

struct StmtBuilder<'a> {
    bld: LLVMBuilderRef,
    tybld: &'a TypeBuilder<'a>,
    llfuncs: &'a [LLVMValueRef],
    llconsts: &'a [LLVMValueRef],

    llfunc: LLVMValueRef,
    locals: &'a [LLVMValueRef],
    sret: Option<LLVMValueRef>,

    break_dest: Vec<LLVMBasicBlockRef>,
    continue_dest: Vec<LLVMBasicBlockRef>,
    block: LLVMBasicBlockRef,
}

#[derive(Debug, Copy, Clone)]
enum Value {
    Unit,
    Scalar(LLVMValueRef),
    Aggregate(LLVMValueRef),
}

impl<'a> StmtBuilder<'a> {
    unsafe fn build_block(&mut self, block: &Block) {
        for stmt in &block.stmts {
            self.build_stmt(stmt);
        }
    }

    unsafe fn position_at_end(&mut self, block: LLVMBasicBlockRef) {
        LLVMPositionBuilderAtEnd(self.bld, block);
        self.block = block;
    }

    unsafe fn build_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Break => {
                let block = match self.break_dest.last() {
                    Some(&b) => b,
                    None => panic!("can't break outside of loop"),
                };
                LLVMBuildBr(self.bld, block);
            }
            Stmt::Continue => {
                let block = match self.continue_dest.last() {
                    Some(&b) => b,
                    None => panic!("can't continue outside of loop"),
                };
                LLVMBuildBr(self.bld, block);
            }
            Stmt::For(init, cond, post, body) => {
                for stmt in init {
                    self.build_stmt(stmt);
                }
                let head = LLVMAppendBasicBlock(self.llfunc, cstr!(""));
                let then = LLVMAppendBasicBlock(self.llfunc, cstr!(""));
                let tail = LLVMAppendBasicBlock(self.llfunc, cstr!(""));
                let done = LLVMAppendBasicBlock(self.llfunc, cstr!(""));
                LLVMBuildBr(self.bld, head);

                self.position_at_end(head);
                let cond = self.build_scalar(cond);
                LLVMBuildCondBr(self.bld, cond, then, done);

                self.position_at_end(then);
                self.break_dest.push(done);
                self.continue_dest.push(tail);
                self.build_block(body);
                self.break_dest.pop();
                self.continue_dest.pop();
                if LLVMGetBasicBlockTerminator(self.block).is_null() {
                    LLVMBuildBr(self.bld, tail);
                }

                self.position_at_end(tail);
                for stmt in post {
                    self.build_stmt(stmt);
                }
                LLVMBuildBr(self.bld, head);

                self.position_at_end(done);
            }
            Stmt::While(cond, body) => {
                let head = LLVMAppendBasicBlock(self.llfunc, cstr!(""));
                let then = LLVMAppendBasicBlock(self.llfunc, cstr!(""));
                let done = LLVMAppendBasicBlock(self.llfunc, cstr!(""));
                LLVMBuildBr(self.bld, head);

                self.position_at_end(head);
                let cond = self.build_scalar(cond);
                LLVMBuildCondBr(self.bld, cond, then, done);

                self.position_at_end(then);
                self.break_dest.push(done);
                self.continue_dest.push(head);
                self.build_block(body);
                self.break_dest.pop();
                self.continue_dest.pop();
                if LLVMGetBasicBlockTerminator(self.block).is_null() {
                    LLVMBuildBr(self.bld, head);
                }

                self.position_at_end(done);
            }
            Stmt::If(cond, body) => {
                let cond = self.build_scalar(cond);
                let then = LLVMAppendBasicBlock(self.llfunc, cstr!(""));
                let done = LLVMAppendBasicBlock(self.llfunc, cstr!(""));
                LLVMBuildCondBr(self.bld, cond, then, done);
                LLVMPositionBuilderAtEnd(self.bld, then);
                self.block = then;
                self.build_block(body);
                if LLVMGetBasicBlockTerminator(self.block).is_null() {
                    LLVMBuildBr(self.bld, done);
                }
                LLVMPositionBuilderAtEnd(self.bld, done);
                self.block = done;
            }
            Stmt::Assign(x, y) => {
                let p = self.build_place(x);
                let _ = self.build_expr(y, Some(p));
            }
            Stmt::Return(x) => {
                let v = self.build_expr(x, self.sret);
                match v {
                    Value::Unit => LLVMBuildRetVoid(self.bld),
                    Value::Aggregate(_) => LLVMBuildRetVoid(self.bld),
                    Value::Scalar(v) => LLVMBuildRet(self.bld, v),
                };
            }
            Stmt::Expr(x) => {
                let _ = self.build_expr(x, None);
            }
        }
    }

    unsafe fn build_place(&mut self, e: &Expr) -> LLVMValueRef {
        match &e.kind {
            &ExprKind::Local(i) => self.locals[i],
            &ExprKind::Param(i) => LLVMGetParam(self.llfunc, i as u32),
            ExprKind::Index(p, i) => {
                let ptr = self.tybld.lltype(p.ty);
                let elem = LLVMGetElementType(ptr);
                let ty = self.tybld.irtype(p.ty);
                let p = match self.tybld.irtype(p.ty).kind() {
                    TypeKind::Aggregate => self.build_place(p),
                    TypeKind::Scalar => {
                        assert_eq!(ty.scalar_kind(), ScalarKind::Pointer);
                        self.build_scalar(p)
                    }
                    TypeKind::Unit => panic!(),
                };
                let i = self.build_scalar(i);
                let mut idx = [i];
                let pidx = idx.as_mut_ptr();
                let nidx = idx.len() as u32;
                LLVMBuildGEP2(self.bld, elem, p, pidx, nidx, cstr!(""))
            }
            &ExprKind::Field(ref x, i) => {
                let ty = self.tybld.irtype(x.ty);
                let (sty_id, p) = match ty {
                    Type::Tuple(_) | Type::Struct(_) => (x.ty, self.build_place(x)),
                    &Type::Pointer(ty) => (ty, self.build_scalar(x)),
                    _ => panic!(),
                };
                let sty = self.tybld.lltype(sty_id);
                LLVMBuildStructGEP2(self.bld, sty, p, i, cstr!(""))
            }
            ExprKind::Unary(Unop::Deref, p) => self.build_scalar(p),
            &ExprKind::Func(i) => self.llfuncs[i],
            &ExprKind::EnumField(ref x, variant, i) => {
                let enty = match self.tybld.irtype(x.ty) {
                    Type::Enum(enty) => enty,
                    _ => panic!(),
                };
                let variant_ty = {
                    let mut xargs = vec![];
                    for &arg in &enty.variants[variant as usize].args {
                        let ty = self.tybld.lltype(arg);
                        xargs.push(ty);
                    }
                    let p = xargs.as_mut_ptr();
                    let n = xargs.len() as u32;
                    LLVMStructType(p, n, 0)
                };
                let ety = self.tybld.lltype(x.ty);
                let enum_ptr = self.build_place(x);
                let body_ptr = LLVMBuildStructGEP2(self.bld, ety, enum_ptr, 0, cstr!(""));
                let variant_ptr = LLVMBuildPointerCast(
                    self.bld,
                    body_ptr,
                    LLVMPointerType(variant_ty, 0),
                    cstr!(""),
                );
                LLVMBuildStructGEP2(self.bld, variant_ty, variant_ptr, i, cstr!(""))
            }
            k => unimplemented!("build place {:?}", k),
        }
    }

    unsafe fn build_expr(&mut self, e: &Expr, dst: Option<LLVMValueRef>) -> Value {
        match self.tybld.irtype(e.ty).kind() {
            TypeKind::Unit => {
                self.build_unit(e);
                Value::Unit
            }
            TypeKind::Aggregate => {
                let p = match dst {
                    Some(p) => p,
                    None => {
                        let sty = self.tybld.lltype(e.ty);
                        LLVMBuildAlloca(self.bld, sty, cstr!(""))
                    }
                };
                self.build_aggregate(e, p);
                Value::Aggregate(p)
            }
            TypeKind::Scalar => {
                let v = self.build_scalar(e);
                if let Some(dst) = dst {
                    LLVMBuildStore(self.bld, v, dst);
                }
                Value::Scalar(v)
            }
        }
    }

    unsafe fn build_call(
        &mut self,
        func: &Expr,
        args: &[Expr],
        sret: Option<LLVMValueRef>,
    ) -> LLVMValueRef {
        let fnty = match self.tybld.irtype(func.ty) {
            &Type::Func(_) => func.ty,
            &Type::Pointer(fnty) => fnty,
            _ => panic!(),
        };
        let fnty = self.tybld.lltype(fnty);
        let func = self.build_scalar(func);
        let mut args2 = vec![];
        for arg in args {
            let arg = self.build_expr(arg, None);
            let arg = match arg {
                Value::Unit => continue,
                Value::Aggregate(p) => p,
                Value::Scalar(v) => v,
            };
            args2.push(arg);
        }
        if let Some(sret) = sret {
            args2.push(sret);
        }
        LLVMBuildCall2(
            self.bld,
            fnty,
            func,
            args2.as_mut_ptr(),
            args2.len() as u32,
            cstr!(""),
        )
    }

    unsafe fn build_unit(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::Unit => {}
            ExprKind::Call(func, args) => {
                let _ = self.build_call(func, args, None);
            }
            _ => panic!("expected (), got {:?}", e),
        }
    }

    unsafe fn build_aggregate(&mut self, e: &Expr, dst: LLVMValueRef) {
        match &e.kind {
            ExprKind::Tuple(elems) => {
                let tuple_ty = self.tybld.lltype(e.ty);
                for (i, e) in elems.iter().enumerate() {
                    let i = i as u32;
                    let dst = LLVMBuildStructGEP2(self.bld, tuple_ty, dst, i, cstr!(""));
                    let _ = self.build_expr(e, Some(dst));
                }
            }
            ExprKind::Struct(fields) => {
                let sty = self.tybld.lltype(e.ty);
                for (i, e) in fields {
                    let i = *i as u32;
                    let dst = LLVMBuildStructGEP2(self.bld, sty, dst, i, cstr!(""));
                    let _ = self.build_expr(e, Some(dst));
                }
            }
            ExprKind::Array(elems) => {
                let aty = self.tybld.lltype(e.ty);
                for (i, e) in elems.iter().enumerate() {
                    let i = i as u64;
                    let z = LLVMConstInt(LLVMInt32Type(), 0, 0);
                    let i = LLVMConstInt(LLVMInt32Type(), i, 0); // FIXME should sign extend?
                    let mut idxs = [z, i];
                    let idxs_ptr = idxs.as_mut_ptr();
                    let idxs_len = idxs.len() as u32;
                    let dst =
                        LLVMBuildInBoundsGEP2(self.bld, aty, dst, idxs_ptr, idxs_len, cstr!(""));
                    let _ = self.build_expr(e, Some(dst));
                }
            }
            ExprKind::Call(func, args) => {
                let _ = self.build_call(func, args, Some(dst));
            }
            ExprKind::Param(i) => {
                let i = *i as u32;
                let param = LLVMGetParam(self.llfunc, i);
                self.copy(e.ty, param, dst);
            }
            ExprKind::Unary(Unop::Deref, p) => {
                let p = self.build_scalar(p);
                self.copy(e.ty, p, dst);
            }
            &ExprKind::EnumCall(variant, ref args) => {
                let ety = self.tybld.lltype(e.ty);
                let tag_index: u32 = if args.len() == 0 { 0 } else { 1 };
                // FIXME This is wrong! ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
                //  This is based on the variant args length. It should be the same index for all
                //  variants, but its not right now.
                let tag_ptr = LLVMBuildStructGEP2(self.bld, ety, dst, tag_index, cstr!(""));
                let tag_value = LLVMConstInt(LLVMInt8Type(), variant as u64, 0);
                LLVMBuildStore(self.bld, tag_value, tag_ptr);
                // No body, skip setting args
                if args.len() == 0 {
                    return;
                }
                let variant_ty = {
                    let enty = match self.tybld.irtype(e.ty) {
                        Type::Enum(enty) => enty,
                        _ => panic!(),
                    };
                    let mut xargs = vec![];
                    for &arg in &enty.variants[variant as usize].args {
                        let ty = self.tybld.lltype(arg);
                        xargs.push(ty);
                    }
                    let p = xargs.as_mut_ptr();
                    let n = xargs.len() as u32;
                    LLVMStructType(p, n, 0)
                };
                let body_ptr = LLVMBuildStructGEP2(self.bld, ety, dst, 0, cstr!(""));
                let variant_ptr = LLVMBuildPointerCast(
                    self.bld,
                    body_ptr,
                    LLVMPointerType(variant_ty, 0),
                    cstr!(""),
                );
                for (i, arg) in args.iter().enumerate() {
                    let i = i as u32;
                    let arg_ptr =
                        LLVMBuildStructGEP2(self.bld, variant_ty, variant_ptr, i, cstr!(""));
                    let _ = self.build_expr(arg, Some(arg_ptr));
                }
            }
            ExprKind::EnumField(_, _, _) => {
                println!("enum field type {:?}", self.tybld.irtype(e.ty));
                unimplemented!()
            }
            ExprKind::Null
            | ExprKind::Unit
            | ExprKind::Integer(_)
            | ExprKind::Float(_)
            | ExprKind::Func(_)
            | ExprKind::Type(_)
            | ExprKind::Unary(_, _)
            | ExprKind::Binary(_, _, _)
            | ExprKind::String(_)
            | ExprKind::Cast(_, _)
            | ExprKind::Bool(_)
            | ExprKind::Char(_)
            | ExprKind::Sizeof(_)
            | ExprKind::EnumVariant(_)
            | ExprKind::EnumTag(_) => {
                panic!("got scalar expression in aggregate place");
            }
            ExprKind::Const(_) => unimplemented!(),
            ExprKind::Field(_, _) | ExprKind::Index(_, _) | ExprKind::Local(_) => {
                let p = self.build_place(e);
                self.copy(e.ty, p, dst);
            }
        }
    }

    unsafe fn copy(&mut self, ty: TypeId, src: LLVMValueRef, dst: LLVMValueRef) {
        let irty = self.tybld.irtype(ty);
        match irty.kind() {
            TypeKind::Unit => {}
            TypeKind::Aggregate => {
                let lltype = self.tybld.lltype(ty);
                let v = LLVMBuildLoad2(self.bld, lltype, src, cstr!(""));
                LLVMBuildStore(self.bld, v, dst);
            }
            TypeKind::Scalar => {
                let lltype = self.tybld.lltype(ty);
                let v = LLVMBuildLoad2(self.bld, lltype, src, cstr!(""));
                LLVMBuildStore(self.bld, v, dst);
            }
        }
    }

    unsafe fn build_scalar(&mut self, e: &Expr) -> LLVMValueRef {
        match &e.kind {
            ExprKind::Index(_, _) | ExprKind::Field(_, _) | ExprKind::EnumField(_, _, _) => {
                let p = self.build_place(e);
                let elem_type = self.tybld.lltype(e.ty);
                LLVMBuildLoad2(self.bld, elem_type, p, cstr!(""))
            }
            ExprKind::Float(s) => {
                let lltype = self.tybld.lltype(e.ty);
                let ptr = s.as_ptr() as *const i8;
                let len = s.len() as u32;
                LLVMConstRealOfStringAndSize(lltype, ptr, len)
            }
            ExprKind::Integer(s) => {
                let lltype = self.tybld.lltype(e.ty);
                let ptr = s.as_ptr() as *const i8;
                let len = s.len() as u32;
                let radix = 10;
                LLVMConstIntOfStringAndSize(lltype, ptr, len, radix)
            }
            ExprKind::Local(i) => {
                let lltype = self.tybld.lltype(e.ty);
                let p = self.locals[*i];
                LLVMBuildLoad2(self.bld, lltype, p, cstr!(""))
            }
            ExprKind::Param(i) => LLVMGetParam(self.llfunc, *i as u32),
            ExprKind::Func(i) => self.llfuncs[*i],
            ExprKind::Binary(op, x, y) => {
                let irty = self.tybld.irtype(x.ty);
                let kind = irty.scalar_kind();
                let x = self.build_scalar(x);
                let y = self.build_scalar(y);
                use Predicate::*;
                use ScalarKind::*;
                match (op, kind) {
                    (Binop::Add, Int) => LLVMBuildAdd(self.bld, x, y, cstr!("")),
                    (Binop::Sub, Int) => LLVMBuildSub(self.bld, x, y, cstr!("")),
                    (Binop::Mul, Int) => LLVMBuildMul(self.bld, x, y, cstr!("")),
                    (Binop::Div, Int) => LLVMBuildSDiv(self.bld, x, y, cstr!("")),
                    (Binop::And, Int) => LLVMBuildAnd(self.bld, x, y, cstr!("")),
                    (Binop::Shl, Int) => LLVMBuildShl(self.bld, x, y, cstr!("")),
                    (Binop::Shr, Int) => LLVMBuildLShr(self.bld, x, y, cstr!("")),

                    (Binop::Add, Float) => LLVMBuildFAdd(self.bld, x, y, cstr!("")),
                    (Binop::Sub, Float) => LLVMBuildFSub(self.bld, x, y, cstr!("")),
                    (Binop::Mul, Float) => LLVMBuildFMul(self.bld, x, y, cstr!("")),
                    (Binop::Div, Float) => LLVMBuildFDiv(self.bld, x, y, cstr!("")),

                    (Binop::Add, Pointer) => {
                        let ptr = self.tybld.lltype(e.ty);
                        let elem = LLVMGetElementType(ptr);
                        let mut idx = [y];
                        let pidx = idx.as_mut_ptr();
                        let nidx = idx.len() as u32;
                        LLVMBuildGEP2(self.bld, elem, x, pidx, nidx, cstr!(""))
                    }

                    (Binop::Sub, Pointer) => LLVMBuildPtrDiff(self.bld, x, y, cstr!("")),

                    (Binop::Cmp(pred), _) => {
                        let pred = match (pred, kind) {
                            (Eq, Float) => LLVMRealPredicate_LLVMRealOEQ,
                            (Ne, Float) => LLVMRealPredicate_LLVMRealONE,
                            (Ge, Float) => LLVMRealPredicate_LLVMRealOGE,
                            (Le, Float) => LLVMRealPredicate_LLVMRealOLE,
                            (Gt, Float) => LLVMRealPredicate_LLVMRealOGT,
                            (Lt, Float) => LLVMRealPredicate_LLVMRealOLT,

                            (Eq, Int) => LLVMIntPredicate_LLVMIntEQ,
                            (Ne, Int) => LLVMIntPredicate_LLVMIntNE,
                            (Ge, Int) => LLVMIntPredicate_LLVMIntSGE,
                            (Le, Int) => LLVMIntPredicate_LLVMIntSLE,
                            (Gt, Int) => LLVMIntPredicate_LLVMIntSGT,
                            (Lt, Int) => LLVMIntPredicate_LLVMIntSLT,

                            (Eq, Pointer) => LLVMIntPredicate_LLVMIntEQ,
                            (Ne, Pointer) => LLVMIntPredicate_LLVMIntNE,
                            (Ge, Pointer) => LLVMIntPredicate_LLVMIntSGE,
                            (Le, Pointer) => LLVMIntPredicate_LLVMIntSLE,
                            (Gt, Pointer) => LLVMIntPredicate_LLVMIntSGT,
                            (Lt, Pointer) => LLVMIntPredicate_LLVMIntSLT,
                        };
                        let cmp = match kind {
                            Float => LLVMBuildFCmp,
                            Int => LLVMBuildICmp,
                            Pointer => LLVMBuildICmp,
                        };
                        cmp(self.bld, pred, x, y, cstr!(""))
                    }
                    (op, kind) => panic!("unimplemented {:?} {:?}", op, kind),
                }
            }
            ExprKind::String(s) => {
                let mut s = unescape(s);
                s.push('\0');
                let ptr = s.as_ptr() as *const i8;
                LLVMBuildGlobalStringPtr(self.bld, ptr, cstr!(""))
            }
            ExprKind::Call(func, args) => self.build_call(func, args, None),
            ExprKind::Cast(e, ty) => {
                let dst_ty = self.tybld.irtype(*ty);
                let src_ty = self.tybld.irtype(e.ty);
                let dst_llty = self.tybld.lltype(*ty);
                let v = self.build_scalar(e);
                match (src_ty, dst_ty) {
                    (Type::I8, Type::I8)
                    | (Type::I16, Type::I16)
                    | (Type::I32, Type::I32)
                    | (Type::I64, Type::I64)
                    | (Type::F32, Type::F32)
                    | (Type::F64, Type::F64) => v,

                    (Type::I8, Type::I16)
                    | (Type::I8, Type::I32)
                    | (Type::I8, Type::I64)
                    | (Type::I16, Type::I32)
                    | (Type::I16, Type::I64)
                    | (Type::I32, Type::I64) => LLVMBuildSExt(self.bld, v, dst_llty, cstr!("")),

                    (Type::I64, Type::I32)
                    | (Type::I64, Type::I16)
                    | (Type::I64, Type::I8)
                    | (Type::I32, Type::I16)
                    | (Type::I32, Type::I8)
                    | (Type::I16, Type::I8) => LLVMBuildTrunc(self.bld, v, dst_llty, cstr!("")),

                    (Type::I32, Type::F32) | (Type::I32, Type::F64) => {
                        LLVMBuildSIToFP(self.bld, v, dst_llty, cstr!(""))
                    }
                    (Type::F32, Type::I32) => LLVMBuildFPToSI(self.bld, v, dst_llty, cstr!("")),

                    (Type::F32, Type::F64) => LLVMBuildFPExt(self.bld, v, dst_llty, cstr!("")),
                    (Type::F64, Type::F32) => LLVMBuildFPTrunc(self.bld, v, dst_llty, cstr!("")),

                    (Type::Pointer(_), Type::Pointer(_)) => {
                        LLVMBuildPointerCast(self.bld, v, dst_llty, cstr!(""))
                    }

                    (x, y) => unimplemented!("{:?} {:?}", x, y),
                }
            }
            ExprKind::Bool(true) => LLVMConstInt(LLVMInt1Type(), 1, 0),
            ExprKind::Bool(false) => LLVMConstInt(LLVMInt1Type(), 0, 0),
            ExprKind::Unary(Unop::AddressOf, e) => self.build_place(e),
            ExprKind::Unary(Unop::Deref, p) => {
                let lltype = self.tybld.lltype(e.ty);
                let p = self.build_scalar(p);
                LLVMBuildLoad2(self.bld, lltype, p, cstr!(""))
            }
            ExprKind::Sizeof(ty) => {
                let lltype = self.tybld.lltype(*ty);
                LLVMSizeOf(lltype)
            }
            ExprKind::Const(i) => self.llconsts[*i],
            ExprKind::Null => {
                let lltype = self.tybld.lltype(e.ty);
                LLVMConstPointerNull(lltype)
            }
            &ExprKind::Char(c) => {
                let lltype = self.tybld.lltype(e.ty);
                LLVMConstInt(lltype, c as u64, 0)
            }
            &ExprKind::EnumVariant(i) => {
                assert_eq!(self.tybld.irtype(e.ty), &Type::I8);
                let lltype = self.tybld.lltype(e.ty);
                LLVMConstInt(lltype, i as u64, 0)
            }
            ExprKind::EnumTag(en) => {
                let p = self.build_place(en);
                let enty = self.tybld.lltype(en.ty);
                let tag_ptr = LLVMBuildStructGEP2(self.bld, enty, p, 1, cstr!(""));
                LLVMBuildLoad2(self.bld, LLVMInt8Type(), tag_ptr, cstr!(""))
            }
            _ => panic!("expected scalar, got {:?}", e),
        }
    }
}

fn unescape(s: &str) -> String {
    let s = &s[1..s.len() - 1];
    let mut x = String::with_capacity(s.len());
    let mut backslash = false;
    for c in s.chars() {
        let escaped = backslash;
        backslash = false;
        let c = match c {
            '\\' if !escaped => {
                backslash = true;
                continue;
            }
            'n' if escaped => '\n',
            't' if escaped => '\t',
            '\\' if escaped => '\\',
            _ => c,
        };
        x.push(c);
    }
    x
}
