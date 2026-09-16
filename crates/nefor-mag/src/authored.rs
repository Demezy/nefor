#[derive(Debug, Clone, PartialEq)]
pub struct Module {
    pub forms: Vec<Form>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Form {
    Require(Require),
    Type(TypeDeclaration),
    Block(BlockItem),
    Invalid(AuthoringError),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Require {
    pub module: String,
    pub exposure: ImportExposure,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ImportExposure {
    Open,
    Qualified,
    Selective(Vec<ImportSelector>),
    NamespaceAlias(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImportSelector {
    pub export: String,
    pub local: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeDeclaration {
    pub name: String,
    pub params: Vec<String>,
    pub body: TypeDeclarationBody,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TypeDeclarationBody {
    Fields(Vec<(String, Type)>),
    TransparentAlias(Type),
    Newtype(Type),
    Adt(Vec<ConstructorDeclaration>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConstructorDeclaration {
    pub name: String,
    pub payload: Type,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BlockItem {
    Let { name: String, value: Expr },
    Expr(Expr),
    Invalid(AuthoringError),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Unit,
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Keyword(String),
    Name(String),
    Vector(Vec<Expr>),
    Fields(Vec<(String, Expr)>),
    If {
        condition: Box<Expr>,
        then_branch: Box<Expr>,
        else_branch: Box<Expr>,
    },
    Construct {
        owner: Type,
        constructor: String,
        payload: Box<Expr>,
    },
    Match {
        value: Box<Expr>,
        arms: Vec<MatchArm>,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
    },
    Function(Function),
    Ascribe {
        target: Type,
        value: Box<Expr>,
    },
    Annotate {
        target: Type,
        value: Box<Expr>,
    },
    TypeTag(Type),
    Invalid(AuthoringError),
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchArm {
    pub constructor: String,
    pub binding: String,
    pub body: Box<Expr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Function {
    pub type_params: Vec<String>,
    pub params: Vec<Parameter>,
    pub result: Type,
    pub body: Vec<BlockItem>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Parameter {
    pub name: String,
    pub ty: Type,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    Name(String),
    Product(Vec<Type>),
    Tag(Box<Type>),
    Function {
        params: Vec<Type>,
        result: Box<Type>,
    },
    Apply {
        constructor: String,
        arguments: Vec<Type>,
    },
    Invalid(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum AuthoringError {
    Type(String),
    Eval(String),
    Arity { expected: usize, got: usize },
}

impl AuthoringError {
    pub fn into_mag_error(self) -> crate::error::MagError {
        match self {
            Self::Type(message) => crate::error::MagError::Type(message),
            Self::Eval(message) => crate::error::MagError::Eval(message),
            Self::Arity { expected, got } => crate::error::MagError::Arity { expected, got },
        }
    }
}
