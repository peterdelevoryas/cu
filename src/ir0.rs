use crate::error;
use crate::String;
use crate::intern;
use crate::print_cursor;
use crate::syntax;

#[derive(Debug, Default)]
pub struct TypeIntern {
    pub types: Vec<Type>,
}

impl TypeIntern {
    fn intern(&mut self, ty: Type) -> TypeId {
        for (i, interned) in self.types.iter().enumerate() {
            if ty == *interned {
                return i
            }
        }
        let i = self.types.len();
        self.types.push(ty);
        i
    }

    fn get(&self, i: TypeId) -> &Type {
        &self.types[i]
    }
}

#[derive(Debug, Default)]
pub struct NameTable {
    names: Vec<(String, Def)>,
}

#[derive(Debug, Copy, Clone)]
enum Def {
    Func(FuncId),
    Type(TypeId),
    Param(ParamId),
    Local(LocalId),
}

impl NameTable {
    fn def(&mut self, name: String, def: Def) {
        self.names.push((name, def));
    }

    fn get(&self, name: String) -> Option<Def> {
        for &(other, def) in &self.names {
            if other == name {
                return Some(def);
            }
        }
        None
    }

    fn enter_scope(&self) -> usize {
        self.names.len()
    }

    fn exit_scope(&mut self, scope: usize) {
        while scope < self.names.len() {
            self.names.pop();
        }
    }
}

pub fn build(funcs: &[syntax::Func]) -> (Vec<FuncDecl>, Vec<FuncBody>, Vec<Type>) {
    let mut b = ModuleBuilder::default();

    b.add_type("i8", Type::I8);
    b.add_type("i32", Type::I32);

    for func in funcs {
        b.add_func_decl(func);
    }

    let mut func_bodys = vec![];
    for id in 0..b.func_decls.len() {
        let body = FuncBody {
            id: id,
            locals: vec![],
            stmts: vec![],
        };
        let b = FuncBuilder {
            module: &mut b,
            body: body,
        };
        func_bodys.push(b.build());
    }

    (b.func_decls, func_bodys, b.types.types)
}

struct FuncBuilder<'a> {
    module: &'a mut ModuleBuilder,
    body: FuncBody,
}

impl<'a> FuncBuilder<'a> {
    fn build(mut self) -> FuncBody {
        self.body
    }
}

#[derive(Default)]
struct ModuleBuilder {
    names: NameTable,
    types: TypeIntern,
    func_decls: Vec<FuncDecl>,
}

impl ModuleBuilder {
    fn add_type(&mut self, name: &str, ty: Type) {
        let name = intern(name);
        let i = self.types.intern(ty);
        self.names.def(name, Def::Type(i));
    }

    fn add_func_decl(&mut self, func: &syntax::Func) {
        let i = self.func_decls.len();
        self.names.def(func.name, Def::Func(i));

        let func_type = self.build_func_type(&func.ty);
        let func_decl = FuncDecl {
            name: func.name,
            ty: func_type,
        };
        self.func_decls.push(func_decl);
    }

    fn build_type(&mut self, ty: &syntax::Type) -> TypeId {
        match ty {
            syntax::Type::Name(name) => match self.names.get(*name) {
                Some(Def::Type(i)) => i,
                _ => panic!(),
            }
            syntax::Type::Pointer(ty) => {
                unimplemented!()
            }
            syntax::Type::Func(ty) => {
                unimplemented!()
            }
        }
    }

    fn build_func_type(&mut self, ty: &syntax::FuncType) -> FuncType {
        let mut params = vec![];
        for ty in &ty.params {
            let ty = self.build_type(ty);
            params.push(ty);
        }
        let ret = self.build_type(&ty.ret);
        FuncType { params, ret }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    I8,
    I32,
    Pointer(TypeId),
    Func(TypeId),
}

pub type TypeId = usize;
pub type ParamId = usize;
pub type FuncId = usize;
pub type LocalId = usize;

#[derive(Debug, Clone, PartialEq)]
pub struct FuncType {
    pub params: Vec<TypeId>,
    pub ret: TypeId,
}

#[derive(Debug)]
pub struct FuncDecl {
    pub name: String,
    pub ty: FuncType,
}

#[derive(Debug)]
pub struct FuncBody {
    pub id: FuncId,
    pub locals: Vec<Type>,
    pub stmts: Vec<Stmt>,
}

#[derive(Debug)]
pub enum Stmt {
    Assign(Expr, Expr),
    Return(Expr),
}

#[derive(Debug)]
pub enum Expr {
    Integer(String),
    Param(ParamId),
    Func(FuncId),
    Type(TypeId),
    Local(LocalId),
}
