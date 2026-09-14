use crate::types::MagType;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BindingId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FrameId(pub u64);

#[derive(Debug, Clone)]
pub struct CheckedExpr {
    pub ty: MagType,
    pub kind: CheckedExprKind,
}

#[derive(Debug, Clone)]
pub enum CheckedExprKind {
    Unit,
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Keyword(String),
    BindingRef(BindingId),
    Vector(Vec<CheckedExpr>),
    Map(Vec<(String, CheckedExpr)>),
    If {
        condition: Box<CheckedExpr>,
        then_branch: Box<CheckedExpr>,
        else_branch: Box<CheckedExpr>,
    },
    Construct {
        owner: MagType,
        constructor: ConstructorDeclarationId,
        payload: Box<CheckedExpr>,
    },
    Match {
        value: Box<CheckedExpr>,
        arms: Vec<CheckedMatchArm>,
    },
    Call {
        callee: Box<CheckedExpr>,
        args: Vec<CheckedExpr>,
    },
    Function(Arc<CheckedFn>),
    Ascribe {
        target: MagType,
        value: Box<CheckedExpr>,
    },
    TypeTag(MagType),
}

#[derive(Debug, Clone)]
pub struct CheckedMatchArm {
    pub constructor: ConstructorDeclarationId,
    pub binding: CheckedParam,
    pub body: Box<CheckedExpr>,
}

#[derive(Debug, Clone)]
pub struct CheckedBlock {
    pub frame_layout: Vec<BindingId>,
    pub bindings: Vec<CheckedBinding>,
    pub expressions: Vec<CheckedExpr>,
}

#[derive(Debug, Clone)]
pub struct CheckedBinding {
    pub id: BindingId,
    pub name: String,
    pub ty: MagType,
    pub initializer: Arc<CheckedExpr>,
}

#[derive(Debug, Clone)]
pub struct CheckedParam {
    pub id: BindingId,
    pub name: String,
    pub ty: MagType,
}

#[derive(Debug, Clone)]
pub struct CheckedFn {
    pub name: Option<String>,
    pub type_params: Vec<String>,
    pub params: Vec<CheckedParam>,
    pub result: MagType,
    pub body: Arc<CheckedBlock>,
}

#[derive(Debug, Clone)]
pub enum BindingSlot {
    Uninitialized(Arc<CheckedExpr>),
    Initializing,
    Ready(Value),
}

#[derive(Debug, Default)]
pub struct ScopeFrame {
    pub names: HashMap<String, Vec<BindingId>>,
    pub slots: HashMap<BindingId, BindingSlot>,
}

pub type Scope = FrameId;

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Symbol(String),
    Keyword(String),
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Nil,
    List(Vec<Expr>),
    Vector(Vec<Expr>),
    Map(Vec<(Expr, Expr)>),
}

impl Expr {
    pub fn as_symbol(&self) -> Option<&str> {
        if let Self::Symbol(s) = self {
            Some(s)
        } else {
            None
        }
    }
    pub fn as_keyword(&self) -> Option<&str> {
        if let Self::Keyword(s) = self {
            Some(s)
        } else {
            None
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        if let Self::Str(s) = self {
            Some(s)
        } else {
            None
        }
    }
    pub fn as_list(&self) -> Option<&[Expr]> {
        if let Self::List(v) = self {
            Some(v)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone)]
pub struct FnValue {
    pub name: Option<String>,
    pub type_params: Vec<String>,
    pub params: Vec<String>,
    pub param_types: Vec<MagType>,
    pub return_type: MagType,
    pub checked: Arc<CheckedFn>,
    pub closure: Vec<Scope>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConstructorDeclarationId {
    pub owner: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConstructorDecl {
    pub id: ConstructorDeclarationId,
    pub payload: MagType,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TypeDeclBody {
    Nominal(MagType),
    Adt(Vec<ConstructorDecl>),
    Native,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TypeDecl {
    pub name: String,
    pub params: Vec<String>,
    pub body: TypeDeclBody,
}

#[derive(Debug, Clone)]
pub enum Value {
    Unit,
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Keyword(String),
    Symbol(String),
    List(Arc<Vec<Value>>),
    Vector(Arc<Vec<Value>>),
    /// A checked ordered product. Unlike a Vector, every position retains its
    /// own trusted type and constructor evidence.
    Product(Arc<Vec<Value>>),
    Map(Arc<BTreeMap<String, Value>>),
    Fn(Arc<FnValue>),
    BuiltinFn(String),
    Type(MagType),
    TypeDecl(TypeDecl),
    TypeTag(crate::types::ConcreteType),
    TypeDescriptor(crate::types::ConcreteType),
    TypeSchema(crate::schema::TypeSchema),
    SemanticTypeId(crate::types::SemanticTypeId),
    PackedValue(Arc<Value>),
    JsonValue(serde_json::Value),
    HostInputs(serde_json::Value),
    Artifact(serde_json::Value),
    Adt {
        owner: MagType,
        constructor: ConstructorDeclarationId,
        payload: Arc<Value>,
    },
    Typed(Arc<Value>, MagType),
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            Self::Typed(value, _) => value.as_str(),
            _ => None,
        }
    }
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Unit => "unit",
            Self::Str(_) => "string",
            Self::Int(_) => "int",
            Self::Float(_) => "float",
            Self::Bool(_) => "bool",
            Self::Keyword(_) => "keyword",
            Self::Symbol(_) => "symbol",
            Self::List(_) => "list",
            Self::Vector(_) => "vector",
            Self::Product(_) => "product",
            Self::Map(_) => "map",
            Self::Fn(_) => "fn",
            Self::BuiltinFn(_) => "builtin-fn",
            Self::Type(_) | Self::TypeDecl(_) => "type",
            Self::TypeTag(_) => "type-tag",
            Self::TypeDescriptor(_) => "type-descriptor",
            Self::TypeSchema(_) => "type-schema",
            Self::SemanticTypeId(_) => "semantic-type-id",
            Self::PackedValue(_) => "packed-value",
            Self::JsonValue(_) => "json-value",
            Self::HostInputs(_) => "host-inputs",
            Self::Artifact(_) => "artifact",
            Self::Adt { .. } => "adt",
            Self::Typed(value, _) => value.type_name(),
        }
    }
}
