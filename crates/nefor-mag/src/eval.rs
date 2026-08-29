use crate::ast::{
    BindingId, CheckedBlock, CheckedExpr, CheckedExprKind, Expr, FnValue, FrameId, TypeDecl, Value,
};
use crate::env::{BindingForce, BindingHandle, Env};
use crate::error::MagError;
use crate::profile::Phase;
use crate::types::{ConcreteType, MagType};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Component, Path, PathBuf};

thread_local! {
    static FORCE_STACK: std::cell::RefCell<Vec<(FrameId, BindingId, String, bool)>> = const { std::cell::RefCell::new(Vec::new()) };
}

struct ForceStackGuard;

impl Drop for ForceStackGuard {
    fn drop(&mut self) {
        FORCE_STACK.with(|stack| {
            stack.borrow_mut().pop();
        });
    }
}

fn enter_force(env: &Env, handle: &BindingHandle) -> Result<ForceStackGuard, MagError> {
    let id = handle.id;
    let frame = handle.frame;
    let name = env
        .binding_metadata(id)
        .map(|binding| binding.name)
        .unwrap_or_else(|| format!("binding#{}", id.0));
    FORCE_STACK.with(|stack| {
        let mut stack = stack.borrow_mut();
        if let Some(start) = stack
            .iter()
            .position(|(active_frame, active, _, initializing)| {
                *active_frame == frame && *active == id && *initializing
            })
        {
            let mut cycle = stack[start..]
                .iter()
                .map(|(_, _, name, _)| name.clone())
                .collect::<Vec<_>>();
            cycle.push(name);
            return Err(MagError::Eval(format!(
                "binding initialization cycle: {}",
                cycle.join(" -> ")
            )));
        }
        stack.push((frame, id, name, true));
        Ok(ForceStackGuard)
    })
}

fn enter_call_binding(env: &Env, id: BindingId) -> Result<ForceStackGuard, MagError> {
    let handle = env.binding_handle(id)?;
    let name = env
        .binding_metadata(id)
        .map(|binding| binding.name)
        .unwrap_or_else(|| format!("binding#{}", id.0));
    FORCE_STACK.with(|stack| stack.borrow_mut().push((handle.frame, id, name, false)));
    Ok(ForceStackGuard)
}

fn force_stack_active() -> bool {
    FORCE_STACK.with(|stack| !stack.borrow().is_empty())
}

pub mod fuel {
    use crate::error::MagError;
    use std::cell::Cell;

    thread_local! {
        static REMAINING: Cell<Option<u64>> = const { Cell::new(None) };
        static CALL_DEPTH: Cell<u16> = const { Cell::new(0) };
        static EXPR_DEPTH: Cell<u16> = const { Cell::new(0) };
        static CALL_LIMIT: Cell<u16> = const { Cell::new(64) };
        static EXPR_LIMIT: Cell<u16> = const { Cell::new(128) };
    }

    pub struct Guard {
        previous: Option<u64>,
        previous_call_limit: u16,
        previous_expr_limit: u16,
        active: bool,
    }
    pub fn install(limits: impl Into<crate::CompilerLimits>) -> Guard {
        let limits = limits.into();
        let previous = REMAINING.with(|remaining| remaining.replace(Some(limits.evaluation_steps)));
        let previous_call_limit = CALL_LIMIT.with(|limit| limit.replace(limits.call_depth));
        let previous_expr_limit = EXPR_LIMIT.with(|limit| limit.replace(limits.expression_depth));
        Guard {
            previous,
            previous_call_limit,
            previous_expr_limit,
            active: true,
        }
    }
    pub fn ensure(limits: impl Into<crate::CompilerLimits>) -> Guard {
        let limits = limits.into();
        REMAINING.with(|remaining| {
            if remaining.get().is_some() {
                Guard {
                    previous: None,
                    previous_call_limit: CALL_LIMIT.with(Cell::get),
                    previous_expr_limit: EXPR_LIMIT.with(Cell::get),
                    active: false,
                }
            } else {
                remaining.set(Some(limits.evaluation_steps));
                let previous_call_limit = CALL_LIMIT.with(|limit| limit.replace(limits.call_depth));
                let previous_expr_limit =
                    EXPR_LIMIT.with(|limit| limit.replace(limits.expression_depth));
                Guard {
                    previous: None,
                    previous_call_limit,
                    previous_expr_limit,
                    active: true,
                }
            }
        })
    }
    pub fn step() -> Result<(), MagError> {
        REMAINING.with(|remaining| match remaining.get() {
            Some(0) => Err(MagError::Budget("expression step limit reached".into())),
            Some(left) => {
                remaining.set(Some(left - 1));
                Ok(())
            }
            None => Err(MagError::Budget(
                "evaluation started without an installed budget".into(),
            )),
        })
    }

    #[cfg(test)]
    pub fn remaining() -> Option<u64> {
        REMAINING.with(Cell::get)
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            if self.active {
                REMAINING.with(|remaining| remaining.set(self.previous));
                CALL_LIMIT.with(|limit| limit.set(self.previous_call_limit));
                EXPR_LIMIT.with(|limit| limit.set(self.previous_expr_limit));
            }
        }
    }

    pub struct CallGuard;
    pub fn enter_call() -> Result<CallGuard, MagError> {
        CALL_DEPTH.with(|depth| {
            let current = depth.get();
            if current >= CALL_LIMIT.with(Cell::get) {
                Err(MagError::Budget("function call depth limit reached".into()))
            } else {
                depth.set(current + 1);
                Ok(CallGuard)
            }
        })
    }
    impl Drop for CallGuard {
        fn drop(&mut self) {
            CALL_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
        }
    }

    pub struct ExprGuard;
    pub fn enter_expr() -> Result<ExprGuard, MagError> {
        EXPR_DEPTH.with(|depth| {
            let current = depth.get();
            if current >= EXPR_LIMIT.with(Cell::get) {
                Err(MagError::Budget("expression nesting limit reached".into()))
            } else {
                depth.set(current + 1);
                Ok(ExprGuard)
            }
        })
    }
    impl Drop for ExprGuard {
        fn drop(&mut self) {
            EXPR_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
        }
    }
}

pub fn eval_program(env: &mut Env, exprs: &[Expr]) -> Result<Value, MagError> {
    let _fuel = fuel::ensure(env.compiler_limits());
    let mut declarations = vec![false; exprs.len()];
    for (index, expr) in exprs.iter().enumerate() {
        let Expr::List(items) = expr else { continue };
        if matches!(items.first(), Some(Expr::Symbol(head)) if matches!(head.as_str(), "require" | "type"))
        {
            eval_expr(env, expr)?;
            declarations[index] = true;
        }
    }
    let source = exprs
        .iter()
        .zip(&declarations)
        .filter_map(|(expr, declaration)| (!declaration).then_some(expr.clone()))
        .collect::<Vec<_>>();
    let checking_started = env.profile_started();
    let checked = crate::checker::compile_block(env, &source)?;
    env.profile_elapsed(Phase::Checking, checking_started);
    let evaluated = eval_checked_block(env, &checked);
    match &evaluated {
        Ok(result) => {
            env.collect_frames(std::slice::from_ref(result));
        }
        Err(_) => {
            env.collect_frames(&[]);
        }
    }
    evaluated
}

fn eval_checked_block(env: &mut Env, block: &CheckedBlock) -> Result<Value, MagError> {
    for binding in &block.bindings {
        env.declare_binding_slot(binding.id, &binding.name, binding.initializer.clone())?;
    }
    for binding in &block.bindings {
        force_binding(env, binding.id)
            .map_err(|error| binding_initialization_error(&binding.name, error))?;
    }
    let mut result = Value::Unit;
    for expression in &block.expressions {
        result = eval_checked_expr(env, expression)?;
    }
    Ok(result)
}

fn force_binding(env: &mut Env, id: crate::ast::BindingId) -> Result<Value, MagError> {
    let handle = env.binding_handle(id)?;
    let _force = match enter_force(env, &handle) {
        Ok(force) => force,
        Err(error) => {
            Env::profile_force_cycle(&handle);
            return Err(error);
        }
    };
    match Env::begin_handle_force(&handle)? {
        BindingForce::Ready(value) => Ok(value),
        BindingForce::Initialize {
            handle,
            initializer,
        } => match eval_checked_expr(env, &initializer) {
            Ok(value) => {
                Env::complete_handle_force(&handle, value.clone())?;
                Ok(value)
            }
            Err(error) => {
                Env::reset_handle_force(&handle, initializer)?;
                Err(error)
            }
        },
    }
}

fn eval_checked_expr(env: &mut Env, expression: &CheckedExpr) -> Result<Value, MagError> {
    let _depth = fuel::enter_expr()?;
    fuel::step()?;
    env.profile_counters(|counters| {
        counters.evaluator_steps = counters.evaluator_steps.saturating_add(1);
    });
    match &expression.kind {
        CheckedExprKind::Unit => Ok(Value::Unit),
        CheckedExprKind::Str(value) => Ok(Value::Str(value.clone())),
        CheckedExprKind::Int(value) => Ok(Value::Int(*value)),
        CheckedExprKind::Float(value) => Ok(Value::Float(*value)),
        CheckedExprKind::Bool(value) => Ok(Value::Bool(*value)),
        CheckedExprKind::Keyword(value) => Ok(Value::Keyword(value.clone())),
        CheckedExprKind::BindingRef(id) => force_binding(env, *id),
        CheckedExprKind::Vector(items) => Ok(Value::Vector(std::sync::Arc::new(
            items
                .iter()
                .map(|item| eval_checked_expr(env, item))
                .collect::<Result<_, _>>()?,
        ))),
        CheckedExprKind::Map(fields) => {
            let fields = fields
                .iter()
                .map(|(name, value)| Ok((name.clone(), eval_checked_expr(env, value)?)))
                .collect::<Result<BTreeMap<_, _>, MagError>>()?;
            Ok(Value::Map(std::sync::Arc::new(fields)))
        }
        CheckedExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            if truthy(&eval_checked_expr(env, condition)?) {
                eval_checked_expr(env, then_branch)
            } else {
                eval_checked_expr(env, else_branch)
            }
        }
        CheckedExprKind::Match { value, arms } => {
            let value = eval_checked_expr(env, value)?;
            let constructor = selected_constructor_type(env, &value)?.ok_or_else(|| {
                MagError::Type("match value lacks selected constructor evidence".into())
            })?;
            let arm = arms
                .iter()
                .find(|arm| nominal_name(&arm.constructor) == nominal_name(&constructor))
                .ok_or_else(|| {
                    MagError::Type(format!(
                        "match has no arm for selected constructor {constructor}"
                    ))
                })?;
            let payload = selected_constructor_value(env, &value, &constructor)?;
            env.push_scope();
            env.define_ready(arm.binding.id, &arm.binding.name, payload);
            let result = eval_checked_expr(env, &arm.body);
            env.pop_scope();
            result
        }
        CheckedExprKind::Call { callee, args } => {
            let function = eval_checked_expr(env, callee)?;
            let args = args
                .iter()
                .map(|argument| eval_checked_expr(env, argument))
                .collect::<Result<Vec<_>, _>>()?;
            let _call_binding = match &callee.kind {
                CheckedExprKind::BindingRef(id) if force_stack_active() => {
                    Some(enter_call_binding(env, *id)?)
                }
                _ => None,
            };
            let resolved_signature = runtime_type(env, &callee.ty);
            apply_resolved(env, &function, &args, &resolved_signature)
        }
        CheckedExprKind::Function(function) => Ok(Value::Fn(std::sync::Arc::new(FnValue {
            name: function.name.clone(),
            type_params: function.type_params.clone(),
            params: function
                .params
                .iter()
                .map(|parameter| parameter.name.clone())
                .collect(),
            param_types: function
                .params
                .iter()
                .map(|parameter| parameter.ty.clone())
                .collect(),
            return_type: function.result.clone(),
            body: vec![],
            checked: Some(function.clone()),
            closure: env.snapshot(),
        }))),
        CheckedExprKind::Ascribe { target, value } => {
            let value = eval_checked_expr(env, value)?;
            checked_typed_value(env, value, runtime_type(env, target))
        }
        CheckedExprKind::TypeTag(ty) => Ok(Value::TypeTag(ConcreteType::resolve(
            env,
            &runtime_type(env, ty),
        )?)),
    }
}

fn runtime_type(env: &Env, ty: &MagType) -> MagType {
    match ty {
        MagType::Var(name) => match env.lookup(name) {
            Ok(Value::Type(ty)) => ty,
            _ => ty.clone(),
        },
        MagType::Named(name, args) => MagType::Named(
            name.clone(),
            args.iter().map(|ty| runtime_type(env, ty)).collect(),
        ),
        MagType::TypeTag(ty) => MagType::TypeTag(Box::new(runtime_type(env, ty))),
        MagType::List(ty) => MagType::List(Box::new(runtime_type(env, ty))),
        MagType::Map(key, value) => MagType::Map(
            Box::new(runtime_type(env, key)),
            Box::new(runtime_type(env, value)),
        ),
        MagType::Record(fields) => MagType::Record(
            fields
                .iter()
                .map(|(name, ty)| (name.clone(), runtime_type(env, ty)))
                .collect(),
        ),
        MagType::Union(types) => {
            MagType::Union(types.iter().map(|ty| runtime_type(env, ty)).collect())
        }
        MagType::Product(types) => {
            MagType::Product(types.iter().map(|ty| runtime_type(env, ty)).collect())
        }
        MagType::Function(params, result) => MagType::Function(
            params.iter().map(|ty| runtime_type(env, ty)).collect(),
            Box::new(runtime_type(env, result)),
        ),
        other => other.clone(),
    }
}

fn binding_initialization_error(name: &str, error: MagError) -> MagError {
    match error {
        MagError::Type(message) => MagError::Type(format!("initializing {name}: {message}")),
        MagError::Eval(message) => MagError::Eval(format!("initializing {name}: {message}")),
        other => other,
    }
}

fn eval_expr(env: &mut Env, expr: &Expr) -> Result<Value, MagError> {
    let _depth = fuel::enter_expr()?;
    fuel::step()?;
    env.profile_counters(|counters| {
        counters.evaluator_steps = counters.evaluator_steps.saturating_add(1);
    });
    match expr {
        Expr::Str(v) => Ok(Value::Str(v.clone())),
        Expr::Int(v) => Ok(Value::Int(*v)),
        Expr::Float(v) => Ok(Value::Float(*v)),
        Expr::Bool(v) => Ok(Value::Bool(*v)),
        Expr::Nil => Ok(Value::Unit),
        Expr::Keyword(v) => Ok(Value::Keyword(v.clone())),
        Expr::Symbol(v) => env.lookup(v),
        Expr::Vector(xs) => Ok(Value::Vector(std::sync::Arc::new(
            xs.iter()
                .map(|x| eval_expr(env, x))
                .collect::<Result<_, _>>()?,
        ))),
        Expr::Map(xs) => eval_map(env, xs),
        Expr::List(xs) => eval_list(env, xs),
    }
}

fn eval_map(env: &mut Env, pairs: &[(Expr, Expr)]) -> Result<Value, MagError> {
    let mut map = BTreeMap::new();
    for (k, v) in pairs {
        let key = match k {
            Expr::Keyword(s) | Expr::Str(s) | Expr::Symbol(s) => s.clone(),
            _ => value_string(&eval_expr(env, k)?),
        };
        map.insert(key, eval_expr(env, v)?);
    }
    Ok(Value::Map(std::sync::Arc::new(map)))
}

fn eval_list(env: &mut Env, items: &[Expr]) -> Result<Value, MagError> {
    if items.is_empty() {
        return Ok(Value::Unit);
    }
    if let Some(result) = maybe_require(env, items) {
        return result;
    }
    if let Expr::Symbol(head) = &items[0] {
        match head.as_str() {
            "fn" => return eval_fn_form(env, &items[1..]),
            "let" => {
                return Err(MagError::Eval(
                    "let is only valid directly in a source or function block".into(),
                ))
            }
            "if" => return eval_if(env, &items[1..]),
            "type" => return eval_type_decl(env, &items[1..]),
            "as" => return eval_as(env, &items[1..]),
            "type-tag" => return eval_type_tag(env, &items[1..]),
            "|" | "+" => {
                return Ok(Value::Type(parse_type(
                    env,
                    &Expr::List(items.to_vec()),
                    &HashSet::new(),
                )?))
            }
            _ => {}
        }
    }
    let args = items[1..]
        .iter()
        .map(|x| eval_expr(env, x))
        .collect::<Result<Vec<_>, _>>()?;
    let f = if let Expr::Symbol(name) = &items[0] {
        let candidates = env.lookup_candidates(name);
        if candidates.len() <= 1 {
            candidates
                .into_iter()
                .next()
                .ok_or_else(|| MagError::Unresolved(name.clone()))?
        } else {
            let mut matching = candidates
                .into_iter()
                .filter(|candidate| match candidate {
                    Value::Fn(function) => crate::checker::check_call(env, function, &args).is_ok(),
                    _ => false,
                })
                .collect::<Vec<_>>();
            if matching.len() > 1 {
                let concrete = matching
                    .iter()
                    .filter(|candidate| matches!(candidate, Value::Fn(function) if function.type_params.is_empty()))
                    .cloned()
                    .collect::<Vec<_>>();
                if concrete.len() == 1 {
                    matching = concrete;
                }
            }
            match matching.as_slice() {
                [function] => function.clone(),
                [] => return Err(MagError::Type(format!("no overload {name} matches call"))),
                _ => {
                    return Err(MagError::Type(format!(
                        "ambiguous overload {name} for call"
                    )))
                }
            }
        }
    } else {
        eval_expr(env, &items[0])?
    };
    apply(env, &f, &args)
}

fn eval_fn_form(env: &mut Env, args: &[Expr]) -> Result<Value, MagError> {
    eval_fn_form_with_binding(env, args, None)
}

fn eval_fn_form_with_binding(
    env: &mut Env,
    args: &[Expr],
    binding: Option<&str>,
) -> Result<Value, MagError> {
    let value = eval_fn_form_with_binding_unchecked(env, args, binding)?;
    let Value::Fn(function) = &value else {
        unreachable!()
    };
    let checking_started = env.profile_started();
    crate::checker::check_function_with_binding(
        env,
        binding,
        &function.params,
        &function.param_types,
        &function.return_type,
        &function.body,
    )?;
    env.profile_elapsed(Phase::Checking, checking_started);
    Ok(value)
}

fn eval_fn_form_with_binding_unchecked(
    env: &mut Env,
    args: &[Expr],
    binding: Option<&str>,
) -> Result<Value, MagError> {
    if args.len() < 4 {
        return Err(MagError::Eval("fn syntax is (fn [[name Type] ...] -> Return body...) or (fn [T ...] [[name Type] ...] -> Return body...)".into()));
    }
    let empty = Expr::Vector(vec![]);
    let (type_expr, param_expr, arrow, return_expr, body) =
        if matches!(args.get(1), Some(Expr::Vector(_))) {
            (&args[0], &args[1], &args[2], &args[3], &args[4..])
        } else {
            (&empty, &args[0], &args[1], &args[2], &args[3..])
        };
    if !matches!(arrow,Expr::Symbol(s) if s=="->") {
        return Err(MagError::Eval(
            "fn signature requires -> before its return type".into(),
        ));
    }
    let type_params = match type_expr {
        Expr::Vector(xs) => xs
            .iter()
            .map(|x| {
                x.as_symbol()
                    .map(str::to_owned)
                    .ok_or_else(|| MagError::Type("fn generic parameters must be symbols".into()))
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => unreachable!(),
    };
    let vars = type_params.iter().cloned().collect::<HashSet<_>>();
    let pairs = match param_expr {
        Expr::Vector(xs) => xs,
        _ => return Err(MagError::Type("fn parameters must be a vector".into())),
    };
    let mut params = vec![];
    let mut param_types = vec![];
    for pair in pairs {
        match pair {
            Expr::Vector(items) if items.len() == 2 => {
                params.push(
                    items[0]
                        .as_symbol()
                        .ok_or_else(|| MagError::Type("fn parameter name must be a symbol".into()))?
                        .to_string(),
                );
                param_types.push(parse_type(env, &items[1], &vars)?);
            }
            _ => {
                return Err(MagError::Type(
                    "fn parameters must be [name Type] pairs".into(),
                ))
            }
        }
    }
    let return_type = parse_type(env, return_expr, &vars)?;
    Ok(Value::Fn(std::sync::Arc::new(FnValue {
        name: binding.map(str::to_owned),
        type_params,
        params,
        param_types,
        return_type,
        body: body.to_vec(),
        checked: None,
        closure: env.snapshot(),
    })))
}

fn eval_as(env: &mut Env, args: &[Expr]) -> Result<Value, MagError> {
    arity(args, 2)?;
    let ty = parse_type(env, &args[0], &HashSet::new())?;
    let value = match &args[1] {
        Expr::Symbol(name) if env.lookup_candidates(name).len() > 1 => {
            env.lookup_by_type(name, &ty)?
        }
        expr => eval_expr(env, expr)?,
    };
    checked_typed_value(env, value, ty)
}

fn eval_type_tag(env: &mut Env, args: &[Expr]) -> Result<Value, MagError> {
    arity(args, 1)?;
    let ty = parse_type(env, &args[0], &HashSet::new())?;
    Ok(Value::TypeTag(crate::types::ConcreteType::resolve(
        env, &ty,
    )?))
}

fn eval_if(env: &mut Env, args: &[Expr]) -> Result<Value, MagError> {
    if !(2..=3).contains(&args.len()) {
        return Err(MagError::Eval(
            "if requires condition, then, and optional else".into(),
        ));
    }
    if truthy(&eval_expr(env, &args[0])?) {
        eval_expr(env, &args[1])
    } else if args.len() == 3 {
        eval_expr(env, &args[2])
    } else {
        Ok(Value::Unit)
    }
}

fn eval_type_decl(env: &mut Env, args: &[Expr]) -> Result<Value, MagError> {
    if args.is_empty() || args.len() > 3 {
        return Err(MagError::Eval(
            "type requires a name, optional generic parameters, and a body".into(),
        ));
    }
    let local = args[0]
        .as_symbol()
        .ok_or_else(|| MagError::Eval("type name must be a symbol".into()))?;
    let (params, body_expr) = match args {
        [_, Expr::Vector(ps), body] => (
            ps.iter()
                .map(|p| {
                    p.as_symbol()
                        .map(str::to_owned)
                        .ok_or_else(|| MagError::Type("generic parameters must be symbols".into()))
                })
                .collect::<Result<Vec<_>, _>>()?,
            Some(body),
        ),
        [_, body] => (vec![], Some(body)),
        [_] => (vec![], None),
        _ => unreachable!(),
    };
    let vars = params.iter().cloned().collect();
    let body = body_expr
        .map(|x| parse_type(env, x, &vars))
        .transpose()?
        .unwrap_or_else(|| MagType::Record(BTreeMap::new()));
    let decl = TypeDecl {
        name: env.qualify(local),
        params,
        body,
    };
    let value = Value::TypeDecl(decl.clone());
    env.define(local, value.clone());
    Ok(value)
}

pub(crate) fn parse_type(
    env: &Env,
    expr: &Expr,
    vars: &HashSet<String>,
) -> Result<MagType, MagError> {
    match expr {
        Expr::Symbol(name) if vars.contains(name) => Ok(MagType::Var(name.clone())),
        Expr::Symbol(name) => {
            let candidates = env.lookup_candidates(name);
            let types = candidates
                .iter()
                .filter_map(|value| match value {
                    Value::Type(ty) => Some(ty.clone()),
                    Value::TypeDecl(decl) => Some(MagType::Named(decl.name.clone(), vec![])),
                    _ => None,
                })
                .collect::<Vec<_>>();
            match types.as_slice() {
                [ty] => Ok(ty.clone()),
                [] if candidates.is_empty() => Err(MagError::Unresolved(name.clone())),
                [] => Err(MagError::Type(format!("{name} is not a type"))),
                _ => Err(MagError::Type(format!("ambiguous type name {name}"))),
            }
        }
        Expr::Map(fields) => {
            let mut out = BTreeMap::new();
            for (k, v) in fields {
                let name = match k {
                    Expr::Keyword(s) | Expr::Symbol(s) | Expr::Str(s) => s.clone(),
                    _ => {
                        return Err(MagError::Type(
                            "record field names must be symbols or keywords".into(),
                        ))
                    }
                };
                out.insert(name, parse_type(env, v, vars)?);
            }
            Ok(MagType::Record(out))
        }
        Expr::List(xs) if !xs.is_empty() => {
            let head = xs[0]
                .as_symbol()
                .ok_or_else(|| MagError::Type("type application head must be a symbol".into()))?;
            match head {
                "|" => Ok(MagType::Union(
                    xs[1..]
                        .iter()
                        .map(|x| parse_type(env, x, vars))
                        .collect::<Result<_, _>>()?,
                )),
                "+" => Ok(MagType::Product(
                    xs[1..]
                        .iter()
                        .map(|x| parse_type(env, x, vars))
                        .collect::<Result<_, _>>()?,
                )),
                "List" if xs.len() == 2 => {
                    Ok(MagType::List(Box::new(parse_type(env, &xs[1], vars)?)))
                }
                "Map" if xs.len() == 3 => Ok(MagType::Map(
                    Box::new(parse_type(env, &xs[1], vars)?),
                    Box::new(parse_type(env, &xs[2], vars)?),
                )),
                "TypeTag" if xs.len() == 2 => {
                    Ok(MagType::TypeTag(Box::new(parse_type(env, &xs[1], vars)?)))
                }
                "Fn" if xs.len() >= 2 => {
                    let types = xs[1..]
                        .iter()
                        .map(|x| parse_type(env, x, vars))
                        .collect::<Result<Vec<_>, _>>()?;
                    let (result, params) = types
                        .split_last()
                        .ok_or_else(|| MagError::Type("Fn requires a return type".into()))?;
                    Ok(MagType::Function(params.to_vec(), Box::new(result.clone())))
                }
                _ => {
                    let decl = match env.lookup(head)? {
                        Value::TypeDecl(d) => d,
                        _ => return Err(MagError::Type(format!("{head} is not a declared type"))),
                    };
                    if decl.params.len() != xs.len() - 1 {
                        return Err(MagError::Type(format!(
                            "{} expects {} type arguments, got {}",
                            decl.name,
                            decl.params.len(),
                            xs.len() - 1
                        )));
                    }
                    Ok(MagType::Named(
                        decl.name.clone(),
                        xs[1..]
                            .iter()
                            .map(|x| parse_type(env, x, vars))
                            .collect::<Result<_, _>>()?,
                    ))
                }
            }
        }
        _ => Err(MagError::Type("invalid type expression".into())),
    }
}

fn record_fields(env: &Env, ty: &MagType) -> Option<BTreeMap<String, MagType>> {
    match ty {
        MagType::Record(fields) => Some(fields.clone()),
        MagType::Named(name, args) => {
            let decl = env.type_decl(name)?;
            let substitutions = decl
                .params
                .iter()
                .cloned()
                .zip(args.iter().cloned())
                .collect();
            let body = crate::checker::substitute(&decl.body, &substitutions);
            record_fields(env, &body)
        }
        _ => None,
    }
}

fn record_field_diff(env: &Env, value: &Value, ty: &MagType) -> Option<String> {
    let Value::Map(actual) = raw(value) else {
        return None;
    };
    let expected = record_fields(env, ty)?;
    let missing = expected
        .keys()
        .filter(|key| !actual.contains_key(*key))
        .cloned()
        .collect::<Vec<_>>();
    let unexpected = actual
        .keys()
        .filter(|key| !expected.contains_key(*key))
        .cloned()
        .collect::<Vec<_>>();
    let invalid = expected
        .iter()
        .filter_map(|(key, ty)| {
            let value = actual.get(key)?;
            validate_value(env, value, ty).is_err().then(|| {
                format!(
                    "{key}: expected {ty}, got {}",
                    crate::checker::value_type(value)
                        .map(|actual| actual.to_string())
                        .unwrap_or_else(|| value.type_name().to_owned())
                )
            })
        })
        .collect::<Vec<_>>();
    if missing.is_empty() && unexpected.is_empty() && invalid.is_empty() {
        return None;
    }
    let mut details = Vec::new();
    if !missing.is_empty() {
        details.push(format!("missing fields: {}", missing.join(", ")));
    }
    if !unexpected.is_empty() {
        details.push(format!("unexpected fields: {}", unexpected.join(", ")));
    }
    if !invalid.is_empty() {
        details.push(format!("invalid fields: {}", invalid.join(", ")));
    }
    Some(details.join("; "))
}

fn checked_typed_value(env: &Env, value: Value, ty: MagType) -> Result<Value, MagError> {
    if let MagType::Product(components) = &ty {
        let values = match raw(&value) {
            Value::List(values) | Value::Vector(values) | Value::Product(values) => values,
            _ => {
                return Err(MagError::Type(format!(
                    "value does not conform to {ty}: expected an ordered tuple"
                )))
            }
        };
        if values.len() != components.len() {
            return Err(MagError::Type(format!(
                "value does not conform to {ty}: expected {} tuple positions, got {}",
                components.len(),
                values.len()
            )));
        }
        let positions = values
            .iter()
            .cloned()
            .zip(components)
            .map(|(position, component)| checked_typed_value(env, position, component.clone()))
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(Value::Product(std::sync::Arc::new(positions)));
    }

    validate_value(env, &value, &ty)?;
    let Ok(accepted) = crate::types::ConcreteType::resolve(env, &ty) else {
        return Ok(Value::Typed(std::sync::Arc::new(value), ty));
    };
    let selected = explicit_constructor(env, &value)?;
    match (&accepted, selected) {
        (crate::types::ConcreteType::Sum { .. }, Some(constructor))
            if accepted.accepts(&constructor) => {}
        (crate::types::ConcreteType::Sum { .. }, Some(constructor)) => {
            return Err(MagError::Type(format!(
                "constructor {constructor:?} is not accepted by {accepted:?}"
            )));
        }
        (crate::types::ConcreteType::Sum { .. }, None) => {
            let source = crate::checker::value_type(&value)
                .map(|actual| actual.to_string())
                .unwrap_or_else(|| value.type_name().to_owned());
            return Err(MagError::Type(format!(
                "cannot construct sum {ty} from source type {source}: the source has no explicit nominal constructor evidence; primitive and structural sum construction is unsupported, so first construct a declared nominal arm with `as`, then refine that value to the sum. This value-construction rule is distinct from graph-edge compatibility"
            )));
        }
        (crate::types::ConcreteType::Named { .. }, Some(constructor))
            if constructor != accepted =>
        {
            return Err(MagError::Type(format!(
                "cannot replace constructor evidence {constructor:?} with {accepted:?}"
            )));
        }
        _ => {}
    }
    Ok(Value::Typed(std::sync::Arc::new(value), ty))
}

fn explicit_constructor(
    env: &Env,
    value: &Value,
) -> Result<Option<crate::types::ConcreteType>, MagError> {
    let mut current = value;
    while let Value::Typed(inner, evidence) = current {
        match crate::types::ConcreteType::resolve(env, evidence)? {
            constructor @ crate::types::ConcreteType::Named { .. } => {
                return Ok(Some(constructor));
            }
            crate::types::ConcreteType::Sum { .. } => current = inner,
            _ => return Ok(None),
        }
    }
    Ok(None)
}

fn selected_constructor_value(
    env: &Env,
    value: &Value,
    selected: &MagType,
) -> Result<Value, MagError> {
    let mut current = value;
    while let Value::Typed(inner, evidence) = current {
        let evidence = runtime_type(env, evidence);
        if runtime_sum_alias(env, &evidence, &mut HashSet::new())? {
            current = inner;
        } else {
            match evidence {
                constructor @ MagType::Named(_, _)
                    if nominal_name(&constructor) == nominal_name(selected) =>
                {
                    return Ok(current.clone());
                }
                constructor @ MagType::Named(_, _) => {
                    return Err(MagError::Type(format!(
                        "selected constructor evidence changed from {selected} to {constructor}"
                    )));
                }
                _ => break,
            }
        }
    }
    Err(MagError::Type(
        "match value lacks selected nominal payload".into(),
    ))
}

fn nominal_name(ty: &MagType) -> Option<&str> {
    match ty {
        MagType::Named(name, _) => Some(name),
        _ => None,
    }
}

fn selected_constructor_type(env: &Env, value: &Value) -> Result<Option<MagType>, MagError> {
    let mut current = value;
    while let Value::Typed(inner, evidence) = current {
        let evidence = runtime_type(env, evidence);
        if runtime_sum_alias(env, &evidence, &mut HashSet::new())? {
            current = inner;
        } else if matches!(evidence, MagType::Named(_, _)) {
            return Ok(Some(evidence));
        } else {
            return Ok(None);
        }
    }
    Ok(None)
}

fn runtime_sum_alias(
    env: &Env,
    ty: &MagType,
    seen: &mut HashSet<String>,
) -> Result<bool, MagError> {
    match ty {
        MagType::Union(_) => Ok(true),
        MagType::Named(name, arguments) => {
            let key = format!("{name}<{arguments:?}>");
            if !seen.insert(key.clone()) {
                return Err(MagError::Type(format!(
                    "recursive sum alias {name} is unsupported"
                )));
            }
            let declaration = env
                .type_decl(name)
                .ok_or_else(|| MagError::Type(format!("unknown nominal type {name}")))?;
            let substitutions = declaration
                .params
                .iter()
                .cloned()
                .zip(arguments.iter().cloned())
                .collect();
            let body = crate::checker::substitute(&declaration.body, &substitutions);
            let result = runtime_sum_alias(env, &body, seen);
            seen.remove(&key);
            result
        }
        _ => Ok(false),
    }
}

fn validate_value(env: &Env, value: &Value, ty: &MagType) -> Result<(), MagError> {
    env.profile_counters(|counters| {
        counters.runtime_value_validation_visits =
            counters.runtime_value_validation_visits.saturating_add(1);
    });
    if let Value::Typed(_, evidence) = value {
        if evidence == ty {
            return Ok(());
        }
        if let (Ok(expected), Ok(actual)) = (
            crate::types::ConcreteType::resolve(env, ty),
            crate::types::ConcreteType::resolve(env, evidence),
        ) {
            if expected.accepts(&actual) {
                return Ok(());
            }
        }
    }
    let original = value;
    let value = raw(value);
    let valid = match ty {
        MagType::Artifact => matches!(value, Value::Artifact(_)),
        MagType::JsonValue => matches!(value, Value::JsonValue(_)),
        MagType::TypeDescriptor => matches!(value, Value::TypeDescriptor(_)),
        MagType::TypeSchema => matches!(value, Value::TypeSchema(_)),
        MagType::SemanticTypeId => matches!(value, Value::SemanticTypeId(_)),
        MagType::PackedValue => matches!(value, Value::PackedValue(_)),
        MagType::HostInputs => matches!(value, Value::HostInputs(_)),
        MagType::Never => false,
        MagType::Unit => matches!(value, Value::Unit),
        MagType::Bool => matches!(value, Value::Bool(_)),
        MagType::Int => matches!(value, Value::Int(_)),
        MagType::Float => matches!(value, Value::Float(_)),
        MagType::String => matches!(value, Value::Str(_)),
        MagType::List(item) => match value {
            Value::List(xs) | Value::Vector(xs) => {
                xs.iter().all(|x| validate_value(env, x, item).is_ok())
            }
            _ => false,
        },
        MagType::EmptyList => {
            matches!(value, Value::List(items) | Value::Vector(items) if items.is_empty())
        }
        MagType::Map(_, item) => match value {
            Value::Map(map) => map.values().all(|x| validate_value(env, x, item).is_ok()),
            _ => false,
        },
        MagType::Record(fields) => match value {
            Value::Map(map) => {
                map.len() == fields.len()
                    && fields.iter().all(|(key, field)| {
                        map.get(key)
                            .is_some_and(|v| validate_value(env, v, field).is_ok())
                    })
            }
            _ => false,
        },
        MagType::Named(name, args) => env.type_decl(name).is_some_and(|decl| {
            let substitutions = decl
                .params
                .iter()
                .cloned()
                .zip(args.iter().cloned())
                .collect();
            let body = crate::checker::substitute(&decl.body, &substitutions);
            validate_value(env, value, &body).is_ok()
        }),
        MagType::TypeTag(expected) => matches!(
            value,
            Value::TypeTag(actual)
                if crate::types::ConcreteType::resolve(env, expected)
                    .is_ok_and(|expected| actual == &expected)
        ),
        MagType::Union(types) => types.iter().any(|t| validate_value(env, value, t).is_ok()),
        MagType::Product(types) => match value {
            Value::List(values) | Value::Vector(values) | Value::Product(values) => {
                values.len() == types.len()
                    && values
                        .iter()
                        .zip(types)
                        .all(|(position, ty)| validate_value(env, position, ty).is_ok())
            }
            _ => false,
        },
        MagType::Function(_, _) => matches!(value, Value::Fn(_)),
        MagType::Var(_) => true,
    };
    if valid {
        Ok(())
    } else if let Some(diff) = record_field_diff(env, original, ty) {
        Err(MagError::Type(format!(
            "value does not conform to {ty}: {diff}"
        )))
    } else {
        Err(MagError::Type(format!("value does not conform to {ty}")))
    }
}

fn apply(caller: &Env, f: &Value, args: &[Value]) -> Result<Value, MagError> {
    apply_with_signature(caller, f, args, None)
}

fn apply_resolved(
    caller: &Env,
    f: &Value,
    args: &[Value],
    resolved_signature: &MagType,
) -> Result<Value, MagError> {
    apply_with_signature(caller, f, args, Some(resolved_signature))
}

fn apply_with_signature(
    caller: &Env,
    f: &Value,
    args: &[Value],
    resolved_signature: Option<&MagType>,
) -> Result<Value, MagError> {
    caller.profile_counters(|counters| {
        counters.function_calls = counters.function_calls.saturating_add(1);
        if matches!(f, Value::BuiltinFn(_)) {
            counters.builtin_calls = counters.builtin_calls.saturating_add(1);
        } else if matches!(f, Value::Fn(_)) {
            counters.user_function_calls = counters.user_function_calls.saturating_add(1);
        }
    });
    match f {
        Value::Fn(fun) => {
            let _call_depth = fuel::enter_call()?;
            if args.len() != fun.params.len() {
                return Err(MagError::Arity {
                    expected: fun.params.len(),
                    got: args.len(),
                });
            }
            let (expected_return, type_bindings) = match resolved_signature {
                Some(signature) => crate::checker::check_resolved_call(caller, fun, signature),
                None => crate::checker::check_call(caller, fun, args),
            }
            .map_err(|error| match &fun.name {
                Some(name) => MagError::Type(format!("calling {name}: {error}")),
                None => error,
            })?;
            if let Some(result) = caller.memoized_call(fun, resolved_signature, args) {
                return Ok(result);
            }
            let mut env = caller.child_for_call();
            env.replace_scopes(fun.closure.clone());
            env.push_scope();
            env.define_type_declarations_from(caller);
            for (name, ty) in type_bindings {
                env.define(&name, Value::Type(ty));
            }
            let evaluated = (|| {
                let out = if let Some(checked) = &fun.checked {
                    for (parameter, value) in checked.params.iter().zip(args) {
                        env.define_ready(parameter.id, &parameter.name, value.clone());
                    }
                    eval_checked_block(&mut env, &checked.body)?
                } else {
                    for (p, v) in fun.params.iter().zip(args) {
                        env.define_binding(p, v.clone())?;
                    }
                    let mut result = Value::Unit;
                    for expression in &fun.body {
                        result = eval_expr(&mut env, expression)?;
                    }
                    result
                };
                validate_value(caller, &out, &expected_return).map_err(|error| {
                    match &fun.name {
                        Some(name) => MagError::Type(format!("returning from {name}: {error}")),
                        None => error,
                    }
                })?;
                checked_typed_value(caller, out, expected_return)
            })();
            drop(env);
            match evaluated {
                Ok(result) => {
                    caller.memoize_call(fun, resolved_signature, args, &result);
                    if caller.frame_collection_due() {
                        let mut roots = Vec::with_capacity(args.len() + 2);
                        roots.push(Value::Fn(fun.clone()));
                        roots.extend_from_slice(args);
                        roots.push(result.clone());
                        caller.collect_frames(&roots);
                    }
                    Ok(result)
                }
                Err(error) => {
                    if caller.frame_collection_due() {
                        let mut roots = Vec::with_capacity(args.len() + 1);
                        roots.push(Value::Fn(fun.clone()));
                        roots.extend_from_slice(args);
                        caller.collect_frames(&roots);
                    }
                    Err(error)
                }
            }
        }
        Value::BuiltinFn(name) => builtin(caller, name, args),
        _ => Err(MagError::Eval(format!("cannot call {}", f.type_name()))),
    }
}

pub fn apply_named(env: &Env, name: &str, arg: Value) -> Result<Value, MagError> {
    let matching = env
        .lookup_candidates(name)
        .into_iter()
        .filter(|candidate| match candidate {
            Value::Fn(function) => {
                crate::checker::check_call(env, function, std::slice::from_ref(&arg)).is_ok()
            }
            Value::BuiltinFn(_) => true,
            _ => false,
        })
        .collect::<Vec<_>>();
    match matching.as_slice() {
        [function] => apply(env, function, &[arg]),
        [] => Err(MagError::Type(format!("no overload {name} matches call"))),
        _ => Err(MagError::Type(format!(
            "ambiguous overload {name} for call"
        ))),
    }
}

pub(crate) fn apply_value(env: &Env, function: &Value, args: &[Value]) -> Result<Value, MagError> {
    apply(env, function, args)
}

fn collection_len(value: &Value) -> Option<u64> {
    match raw(value) {
        Value::List(values) | Value::Vector(values) | Value::Product(values) => {
            Some(values.len() as u64)
        }
        Value::Map(values) => Some(values.len() as u64),
        _ => None,
    }
}

fn builtin(env: &Env, name: &str, args: &[Value]) -> Result<Value, MagError> {
    let input_items = match name {
        "concat" => args.iter().filter_map(collection_len).sum(),
        "remove-at" | "descriptor-table" => args.first().and_then(collection_len).unwrap_or(0),
        "descriptor-input-assignments" => args.get(1).and_then(collection_len).unwrap_or(0),
        "fold" => args.get(2).and_then(collection_len).unwrap_or(0),
        "map" | "indexed-map" | "filter" | "flat-map" | "sort-by" => {
            args.get(1).and_then(collection_len).unwrap_or(0)
        }
        _ => 0,
    };
    env.profile_counters(|counters| {
        *counters
            .builtin_calls_by_name
            .entry(name.to_owned())
            .or_default() += 1;
        if input_items > 0
            || matches!(
                name,
                "concat"
                    | "remove-at"
                    | "descriptor-table"
                    | "descriptor-input-assignments"
                    | "fold"
                    | "map"
                    | "indexed-map"
                    | "filter"
                    | "flat-map"
                    | "sort-by"
            )
        {
            *counters
                .builtin_input_items_by_name
                .entry(name.to_owned())
                .or_default() += input_items;
        }
    });
    match name {
        "artifact" => {
            arity(args, 1)?;
            Ok(Value::Artifact(crate::json::value_to_json(env, &args[0])?))
        }
        "str" => Ok(Value::Str(
            args.iter().map(value_string).collect::<Vec<_>>().join(""),
        )),
        "strip-margin" => {
            arity(args, 1)?;
            let value = raw(&args[0])
                .as_str()
                .ok_or_else(|| MagError::Type("strip-margin expects a String".into()))?;
            Ok(Value::Str(strip_margin(value)))
        }
        "replace" => {
            arity(args, 3)?;
            let value = raw(&args[0])
                .as_str()
                .ok_or_else(|| MagError::Type("replace expects a String".into()))?;
            let from = raw(&args[1])
                .as_str()
                .ok_or_else(|| MagError::Type("replace expects a String pattern".into()))?;
            let to = raw(&args[2])
                .as_str()
                .ok_or_else(|| MagError::Type("replace expects a String replacement".into()))?;
            Ok(Value::Str(value.replace(from, to)))
        }
        "canonical" => {
            arity(args, 1)?;
            let json = canonical_json(crate::json::value_to_json(env, &args[0])?);
            serde_json::to_string(&json)
                .map(Value::Str)
                .map_err(|error| MagError::Eval(format!("canonical serialization failed: {error}")))
        }
        "function-name" => {
            arity(args, 1)?;
            match raw(&args[0]) {
                Value::Fn(function) => function.name.clone().map(Value::Str).ok_or_else(|| {
                    MagError::Eval(
                        "function-name requires a function bound by let; anonymous closures have no resident identity"
                            .into(),
                    )
                }),
                _ => Err(MagError::Type("function-name expects a function".into())),
            }
        }
        "conforms?" => {
            arity(args, 2)?;
            let ty = match raw(&args[1]) {
                Value::TypeDescriptor(ty) => ty.to_mag_type(),
                _ => return Err(MagError::Type("conforms? expects a TypeDescriptor".into())),
            };
            let Ok(schema) = crate::schema::TypeSchema::reify(env, &ty) else {
                return Ok(Value::Bool(false));
            };
            let value = crate::json::value_to_json(env, &args[0])?;
            let encoded = serde_json::to_string(&value)
                .map_err(|error| MagError::Eval(format!("serialize conformance value: {error}")))?;
            Ok(Value::Bool(schema.validate_json(&encoded).ok))
        }
        "count" => {
            arity(args, 1)?;
            let n = match raw(&args[0]) {
                Value::List(v) | Value::Vector(v) => v.len(),
                Value::Map(v) => v.len(),
                Value::Str(v) => v.chars().count(),
                _ => return Err(MagError::Eval("count expects a collection".into())),
            };
            Ok(Value::Int(n as i64))
        }
        "remove-at" => {
            arity(args, 2)?;
            let mut values = match raw(&args[0]) {
                Value::List(values) | Value::Vector(values) => values.as_ref().clone(),
                _ => return Err(MagError::Eval("remove-at expects List".into())),
            };
            let index = match raw(&args[1]) {
                Value::Int(index) if *index >= 0 => *index as usize,
                _ => {
                    return Err(MagError::Eval(
                        "remove-at index must be non-negative Int".into(),
                    ))
                }
            };
            if index >= values.len() {
                return Err(MagError::Eval(format!(
                    "remove-at index {index} is out of bounds for {} values",
                    values.len()
                )));
            }
            values.remove(index);
            Ok(Value::Vector(std::sync::Arc::new(values)))
        }
        "get" => {
            arity(args, 2)?;
            let key = value_string(&args[1]);
            match raw(&args[0]) {
                Value::Map(m) => Ok(m
                    .get(key.trim_start_matches(':'))
                    .cloned()
                    .unwrap_or(Value::Unit)),
                _ => Err(MagError::Eval("get expects a map".into())),
            }
        }
        "assoc" => {
            arity(args, 3)?;
            let mut m = match raw(&args[0]) {
                Value::Map(m) => m.as_ref().clone(),
                _ => return Err(MagError::Eval("assoc expects a map".into())),
            };
            m.insert(
                value_string(&args[1]).trim_start_matches(':').into(),
                args[2].clone(),
            );
            Ok(Value::Map(std::sync::Arc::new(m)))
        }
        "keys" => {
            arity(args, 1)?;
            match raw(&args[0]) {
                Value::Map(m) => Ok(Value::Vector(std::sync::Arc::new(
                    m.keys().cloned().map(Value::Str).collect(),
                ))),
                _ => Err(MagError::Eval("keys expects a map".into())),
            }
        }
        "first" => {
            arity(args, 1)?;
            match raw(&args[0]) {
                Value::List(values) | Value::Vector(values) => values
                    .first()
                    .cloned()
                    .ok_or_else(|| MagError::Eval("first expects a non-empty List".into())),
                _ => Err(MagError::Eval("first expects a List".into())),
            }
        }
        "concat" => {
            arity(args, 2)?;
            match (raw(&args[0]), raw(&args[1])) {
                (Value::Str(a), Value::Str(b)) => Ok(Value::Str(format!("{a}{b}"))),
                (Value::Vector(a), Value::Vector(b)) => Ok(Value::Vector(std::sync::Arc::new(
                    a.iter().chain(b.iter()).cloned().collect(),
                ))),
                (Value::List(a), Value::List(b)) => Ok(Value::List(std::sync::Arc::new(
                    a.iter().chain(b.iter()).cloned().collect(),
                ))),
                _ => Err(MagError::Eval(
                    "concat expects matching strings or collections".into(),
                )),
            }
        }
        "=" => {
            arity(args, 2)?;
            Ok(Value::Bool(equal(env, &args[0], &args[1])))
        }
        "fail" => {
            arity(args, 1)?;
            let diagnostic = crate::json::value_to_json(env, &args[0])?;
            Err(MagError::Eval(format!("validation failed: {diagnostic}")))
        }
        "host-input" => {
            arity(args, 2)?;
            let key = raw(&args[0])
                .as_str()
                .ok_or_else(|| MagError::Type("host-input key must be a String".into()))?;
            let Value::TypeTag(expected) = raw(&args[1]) else {
                return Err(MagError::Type("host-input expects a TypeTag".into()));
            };
            let host_inputs = env.lookup_by_type("inputs", &MagType::HostInputs)?;
            let Value::HostInputs(inputs) = raw(&host_inputs) else {
                return Err(MagError::Type(
                    "host-input requires compiler host inputs".into(),
                ));
            };
            let value = inputs
                .get(key)
                .ok_or_else(|| MagError::Type(format!("host input {key:?} is not present")))?;
            crate::json::json_to_typed_value(env, value, &expected.to_mag_type())
                .map_err(|error| MagError::Type(format!("host input {key:?}: {error}")))
        }
        "type-evidence" => {
            arity(args, 1)?;
            match raw(&args[0]) {
                Value::TypeTag(ty) => Ok(Value::TypeDescriptor(ty.clone())),
                other => Err(MagError::Type(format!(
                    "type-evidence expects TypeTag, got {}",
                    other.type_name()
                ))),
            }
        }
        "type-schema" => {
            arity(args, 1)?;
            let ty = match raw(&args[0]) {
                Value::TypeTag(ty) => ty,
                other => {
                    return Err(MagError::Type(format!(
                        "type-schema expects TypeTag, got {}",
                        other.type_name()
                    )))
                }
            };
            let schema = crate::schema::TypeSchema::reify(env, &ty.to_mag_type())?;
            Ok(Value::TypeSchema(schema))
        }
        "type-id" => {
            arity(args, 1)?;
            let Value::TypeDescriptor(ty) = raw(&args[0]) else {
                return Err(MagError::Type("type-id expects a TypeDescriptor".into()));
            };
            Ok(Value::SemanticTypeId(ty.stable_id()))
        }
        "value-type-id" => {
            arity(args, 2)?;
            let declared = declared_value_type(&args[1])?;
            let selected = selected_value_type(env, &args[0], declared)?;
            Ok(Value::SemanticTypeId(selected.stable_id()))
        }
        "value-type-evidence" => {
            arity(args, 2)?;
            let Value::TypeDescriptor(declared) = raw(&args[1]) else {
                return Err(MagError::Type(
                    "value-type-evidence expects a TypeDescriptor".into(),
                ));
            };
            Ok(Value::TypeDescriptor(selected_value_type(
                env, &args[0], declared,
            )?))
        }
        "pack" => {
            arity(args, 1)?;
            Ok(Value::PackedValue(std::sync::Arc::new(args[0].clone())))
        }
        "packed-empty-record?" => {
            arity(args, 1)?;
            let Value::PackedValue(value) = raw(&args[0]) else {
                return Err(MagError::Type(
                    "packed-empty-record? expects PackedValue".into(),
                ));
            };
            Ok(Value::Bool(
                matches!(raw(value), Value::Map(fields) if fields.is_empty()),
            ))
        }
        "packed-record-has-only-key?" => {
            arity(args, 2)?;
            let Value::PackedValue(value) = raw(&args[0]) else {
                return Err(MagError::Type(
                    "packed-record-has-only-key? expects PackedValue".into(),
                ));
            };
            let key = args[1]
                .as_str()
                .ok_or_else(|| MagError::Type("packed record key must be String".into()))?;
            Ok(Value::Bool(matches!(
                raw(value),
                Value::Map(fields) if fields.len() == 1 && fields.contains_key(key)
            )))
        }
        "packed-record-has-only-keys?" => {
            arity(args, 2)?;
            let Value::PackedValue(value) = raw(&args[0]) else {
                return Err(MagError::Type(
                    "packed-record-has-only-keys? expects PackedValue".into(),
                ));
            };
            let values = match raw(&args[1]) {
                Value::List(values) | Value::Vector(values) => values,
                _ => {
                    return Err(MagError::Type(
                        "packed-record-has-only-keys? expects a String list".into(),
                    ))
                }
            };
            let keys = values
                .iter()
                .map(|value| {
                    value.as_str().ok_or_else(|| {
                        MagError::Type("packed-record-has-only-keys? expects a String list".into())
                    })
                })
                .collect::<Result<BTreeSet<_>, _>>()?;
            Ok(Value::Bool(matches!(
                raw(value),
                Value::Map(fields)
                    if fields.len() == keys.len()
                        && fields.keys().all(|key| keys.contains(key.as_str()))
            )))
        }
        "packed-field-conforms?" => {
            arity(args, 3)?;
            let Value::PackedValue(value) = raw(&args[0]) else {
                return Err(MagError::Type(
                    "packed-field-conforms? expects PackedValue".into(),
                ));
            };
            let key = args[1]
                .as_str()
                .ok_or_else(|| MagError::Type("packed record key must be String".into()))?;
            let Value::TypeDescriptor(ty) = raw(&args[2]) else {
                return Err(MagError::Type(
                    "packed-field-conforms? expects TypeDescriptor".into(),
                ));
            };
            let valid = match raw(value) {
                Value::Map(fields) => fields
                    .get(key)
                    .is_some_and(|field| validate_value(env, field, &ty.to_mag_type()).is_ok()),
                _ => false,
            };
            Ok(Value::Bool(valid))
        }
        "descriptor-accepts?" | "descriptor-accepts-value?" => {
            arity(args, 2)?;
            let Value::TypeDescriptor(target) = raw(&args[0]) else {
                return Err(MagError::Type(format!(
                    "{name} expects TypeDescriptor arguments"
                )));
            };
            let Value::TypeDescriptor(source) = raw(&args[1]) else {
                return Err(MagError::Type(format!(
                    "{name} expects TypeDescriptor arguments"
                )));
            };
            Ok(Value::Bool(if name == "descriptor-accepts-value?" {
                target.accepts(source)
            } else {
                target.accepts_edge_source(source)
            }))
        }
        "descriptor-input-covered-by?" => {
            arity(args, 2)?;
            let Value::TypeDescriptor(target) = raw(&args[0]) else {
                return Err(MagError::Type(
                    "descriptor-input-covered-by? expects a TypeDescriptor target".into(),
                ));
            };
            let sources = match raw(&args[1]) {
                Value::List(sources) | Value::Vector(sources) => sources,
                _ => {
                    return Err(MagError::Type(
                        "descriptor-input-covered-by? expects a descriptor list".into(),
                    ))
                }
            };
            let sources = sources
                .iter()
                .map(|source| match raw(source) {
                    Value::TypeDescriptor(source) => Ok(source.clone()),
                    _ => Err(MagError::Type(
                        "descriptor-input-covered-by? expects a descriptor list".into(),
                    )),
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Value::Bool(target.input_is_covered_by(&sources)))
        }
        "descriptor-input-assignments" => {
            arity(args, 2)?;
            let Value::TypeDescriptor(target) = raw(&args[0]) else {
                return Err(MagError::Type(
                    "descriptor-input-assignments expects a TypeDescriptor target".into(),
                ));
            };
            let sources = descriptor_list(
                &args[1],
                "descriptor-input-assignments expects a descriptor list",
            )?;
            target
                .assign_input_sources(&sources)
                .map(|assignments| {
                    Value::List(std::sync::Arc::new(
                        assignments
                            .into_iter()
                            .map(|position| {
                                Value::Int(position.map_or(-1, |position| position as i64))
                            })
                            .collect(),
                    ))
                })
                .map_err(|error| MagError::Type(error.to_string()))
        }
        "descriptor-output-covered-by?" => {
            arity(args, 2)?;
            let Value::TypeDescriptor(target) = raw(&args[0]) else {
                return Err(MagError::Type(
                    "descriptor-output-covered-by? expects a TypeDescriptor target".into(),
                ));
            };
            let handlers = descriptor_list(
                &args[1],
                "descriptor-output-covered-by? expects a descriptor list",
            )?;
            Ok(Value::Bool(target.output_is_covered_by(&handlers)))
        }
        "descriptor-table" => {
            arity(args, 1)?;
            let descriptors =
                descriptor_list(&args[0], "descriptor-table expects a descriptor list")?;
            let mut declarations = BTreeMap::new();
            for descriptor in descriptors {
                for (id, declaration) in descriptor.declarations()? {
                    if let Some(existing) = declarations.insert(id.clone(), declaration.clone()) {
                        if existing != declaration {
                            return Err(MagError::Type(format!(
                                "semantic type identity collision at {id}"
                            )));
                        }
                    }
                }
            }
            Ok(Value::Map(std::sync::Arc::new(
                declarations
                    .into_iter()
                    .map(|(id, descriptor)| (id, Value::TypeDescriptor(descriptor)))
                    .collect(),
            )))
        }
        "not" => {
            arity(args, 1)?;
            Ok(Value::Bool(!truthy(&args[0])))
        }
        "or" => {
            arity(args, 2)?;
            Ok(if truthy(&args[0]) {
                args[0].clone()
            } else {
                args[1].clone()
            })
        }
        "map" | "indexed-map" | "filter" | "flat-map" | "fold" | "sort-by" => {
            collection_builtin(env, name, args)
        }
        "read" => {
            if args.is_empty() || args.len() > 2 {
                return Err(MagError::Eval(
                    "read requires path and optional interpolation map".into(),
                ));
            }
            let path = args[0]
                .as_str()
                .ok_or_else(|| MagError::Eval("read path must be a string".into()))?;
            let full = resolve_workspace_path(env.source_dir(), path)?;
            let mut s = env.read_file(&full, path)?;
            if let Some(Value::Map(m)) = args.get(1) {
                for (k, v) in m.iter() {
                    s = s.replace(&format!("{{{{{k}}}}}"), &value_string(v));
                }
            }
            Ok(Value::Str(s))
        }
        "read-json" => {
            arity(args, 1)?;
            let path = args[0]
                .as_str()
                .ok_or_else(|| MagError::Eval("read-json path must be a string".into()))?;
            let mut matches = std::iter::once(env.source_dir())
                .chain(env.module_roots().iter().map(PathBuf::as_path))
                .filter_map(|root| {
                    let candidate = resolve_workspace_path(root, path).ok()?;
                    candidate
                        .is_file()
                        .then(|| candidate.canonicalize().unwrap_or(candidate))
                })
                .collect::<Vec<_>>();
            matches.sort();
            matches.dedup();
            let full = match matches.as_slice() {
                [path] => path,
                [] => {
                    return Err(MagError::Eval(format!(
                        "cannot find JSON data {path} in source or module roots"
                    )))
                }
                paths => {
                    return Err(MagError::Eval(format!(
                        "JSON data {path} is ambiguous across source and module roots: {}",
                        paths
                            .iter()
                            .map(|path| path.display().to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )))
                }
            };
            let source = env.read_file(full, path)?;
            let value = serde_json::from_str(&source)
                .map_err(|error| MagError::Eval(format!("cannot parse JSON {path}: {error}")))?;
            Ok(crate::json::json_to_value(&value))
        }
        "require" => Err(MagError::Eval("require is a special form".into())),
        _ => Err(MagError::Eval(format!("unknown builtin {name}"))),
    }
}

fn canonical_json(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(canonical_json).collect())
        }
        serde_json::Value::Object(fields) => {
            let sorted = fields
                .into_iter()
                .map(|(key, value)| (key, canonical_json(value)))
                .collect::<BTreeMap<_, _>>();
            serde_json::Value::Object(sorted.into_iter().collect())
        }
        scalar => scalar,
    }
}

fn collection_builtin(env: &Env, name: &str, args: &[Value]) -> Result<Value, MagError> {
    let seq = |v: &Value| match raw(v) {
        Value::List(v) | Value::Vector(v) => Ok(v.clone()),
        _ => Err(MagError::Eval(format!("{name} expects a collection"))),
    };
    match name {
        "map" => {
            arity(args, 2)?;
            Ok(Value::Vector(std::sync::Arc::new(
                seq(&args[1])?
                    .iter()
                    .map(|v| apply(env, &args[0], std::slice::from_ref(v)))
                    .collect::<Result<_, _>>()?,
            )))
        }
        "indexed-map" => {
            arity(args, 2)?;
            Ok(Value::Vector(std::sync::Arc::new(
                seq(&args[1])?
                    .iter()
                    .cloned()
                    .enumerate()
                    .map(|(index, value)| apply(env, &args[0], &[Value::Int(index as i64), value]))
                    .collect::<Result<_, _>>()?,
            )))
        }
        "filter" => {
            arity(args, 2)?;
            let mut out = vec![];
            for v in seq(&args[1])?.iter().cloned() {
                if truthy(&apply(env, &args[0], std::slice::from_ref(&v))?) {
                    out.push(v)
                }
            }
            Ok(Value::Vector(std::sync::Arc::new(out)))
        }
        "flat-map" => {
            arity(args, 2)?;
            let mut out = vec![];
            for v in seq(&args[1])?.iter().cloned() {
                out.extend(seq(&apply(env, &args[0], &[v])?)?.iter().cloned())
            }
            Ok(Value::Vector(std::sync::Arc::new(out)))
        }
        "fold" => {
            arity(args, 3)?;
            let mut acc = args[1].clone();
            for v in seq(&args[2])?.iter().cloned() {
                acc = apply(env, &args[0], &[acc, v])?;
            }
            Ok(acc)
        }
        "sort-by" => {
            arity(args, 2)?;
            let mut keyed = seq(&args[1])?
                .iter()
                .cloned()
                .map(|value| {
                    let key = apply(env, &args[0], std::slice::from_ref(&value))?;
                    let key = key
                        .as_str()
                        .ok_or_else(|| {
                            MagError::Eval("sort-by callback must return String".into())
                        })?
                        .to_owned();
                    Ok((key, value))
                })
                .collect::<Result<Vec<_>, MagError>>()?;
            keyed.sort_by(|left, right| left.0.cmp(&right.0));
            Ok(Value::Vector(std::sync::Arc::new(
                keyed.into_iter().map(|(_, value)| value).collect(),
            )))
        }
        _ => unreachable!(),
    }
}

fn descriptor_list(value: &Value, error: &str) -> Result<Vec<ConcreteType>, MagError> {
    let values = match raw(value) {
        Value::List(values) | Value::Vector(values) => values,
        _ => return Err(MagError::Type(error.into())),
    };
    values
        .iter()
        .map(|value| match raw(value) {
            Value::TypeDescriptor(descriptor) => Ok(descriptor.clone()),
            _ => Err(MagError::Type(error.into())),
        })
        .collect()
}

fn arity<T>(args: &[T], expected: usize) -> Result<(), MagError> {
    if args.len() == expected {
        Ok(())
    } else {
        Err(MagError::Arity {
            expected,
            got: args.len(),
        })
    }
}
fn truthy(v: &Value) -> bool {
    !matches!(raw(v), Value::Unit | Value::Bool(false))
}
fn value_string(v: &Value) -> String {
    match raw(v) {
        Value::Unit => "()".into(),
        Value::Str(s) | Value::Symbol(s) => s.clone(),
        Value::Keyword(s) => format!(":{s}"),
        Value::Int(n) => n.to_string(),
        Value::Float(n) => n.to_string(),
        Value::Bool(n) => n.to_string(),
        Value::Type(t) => t.to_string(),
        Value::TypeDecl(d) => d.name.clone(),
        Value::TypeTag(t) => t.to_mag_type().to_string(),
        _ => format!("<{:?}>", v.type_name()),
    }
}

fn strip_margin(value: &str) -> String {
    value
        .split_inclusive('\n')
        .map(|line| {
            let margin = line.char_indices().find_map(|(index, character)| {
                if character == '|' {
                    Some(Some(index + character.len_utf8()))
                } else if character.is_whitespace() {
                    None
                } else {
                    Some(None)
                }
            });
            match margin.flatten() {
                Some(content_start) => &line[content_start..],
                None => line,
            }
        })
        .collect()
}
pub(crate) fn equal(env: &Env, a: &Value, b: &Value) -> bool {
    env.profile_counters(|counters| {
        counters.value_equality_visits = counters.value_equality_visits.saturating_add(1);
    });
    match (raw(a), raw(b)) {
        (Value::Unit, Value::Unit) => true,
        (Value::Str(a), Value::Str(b)) => a == b,
        (Value::Int(a), Value::Int(b)) => a == b,
        (Value::Float(a), Value::Float(b)) => a == b,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Keyword(a), Value::Keyword(b)) => a == b,
        (Value::Symbol(a), Value::Symbol(b)) => a == b,
        (Value::BuiltinFn(a), Value::BuiltinFn(b)) => a == b,
        (Value::Fn(a), Value::Fn(b)) => std::sync::Arc::ptr_eq(a, b),
        (Value::List(a), Value::List(b))
        | (Value::Vector(a), Value::Vector(b))
        | (Value::Product(a), Value::Product(b)) => {
            a.len() == b.len()
                && a.iter()
                    .zip(b.iter())
                    .all(|(left, right)| equal(env, left, right))
        }
        (Value::Map(a), Value::Map(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, value)| b.get(key).is_some_and(|other| equal(env, value, other)))
        }
        (Value::Type(a), Value::Type(b)) => a == b,
        (Value::TypeTag(a), Value::TypeTag(b)) => a == b,
        (Value::TypeDecl(a), Value::TypeDecl(b)) => a == b,
        (Value::TypeDescriptor(a), Value::TypeDescriptor(b)) => a == b,
        (Value::TypeSchema(a), Value::TypeSchema(b)) => a == b,
        (Value::SemanticTypeId(a), Value::SemanticTypeId(b)) => a == b,
        (Value::PackedValue(a), Value::PackedValue(b)) => equal(env, a, b),
        (Value::JsonValue(a), Value::JsonValue(b)) => a == b,
        (Value::HostInputs(a), Value::HostInputs(b)) => a == b,
        (Value::Artifact(a), Value::Artifact(b)) => a == b,
        _ => false,
    }
}

fn raw(value: &Value) -> &Value {
    match value {
        Value::Typed(inner, _) => raw(inner),
        other => other,
    }
}

fn declared_value_type(value: &Value) -> Result<&ConcreteType, MagError> {
    match raw(value) {
        Value::TypeTag(declared) | Value::TypeDescriptor(declared) => Ok(declared),
        _ => Err(MagError::Type(
            "value type evidence must be TypeTag or TypeDescriptor".into(),
        )),
    }
}

fn selected_value_type(
    env: &Env,
    value: &Value,
    declared: &ConcreteType,
) -> Result<ConcreteType, MagError> {
    if matches!(declared, ConcreteType::Sum { .. }) {
        explicit_constructor(env, value)?
            .ok_or_else(|| MagError::Type("sum value lacks selected constructor evidence".into()))
    } else {
        Ok(declared.clone())
    }
}

fn module_path(name: &str) -> Result<String, MagError> {
    if name.split('.').any(|p| {
        p.is_empty()
            || !p
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    }) {
        return Err(MagError::Eval(format!("invalid module name: {name}")));
    }
    Ok(format!("{}.mag", name.replace('.', "/")))
}

fn eval_require(env: &mut Env, name: &str) -> Result<Value, MagError> {
    if let Some(defs) = env.module_cached(name) {
        env.install_module(name, defs.clone());
        return Ok(Value::Map(std::sync::Arc::new(module_value_map(&defs))));
    }
    env.begin_module(name)?;
    let resolve_started = env.profile_started();
    let relative = module_path(name)?;
    let mut matches = env
        .module_roots()
        .iter()
        .filter_map(|root| {
            let path = resolve_workspace_path(root, &relative).ok()?;
            path.is_file().then(|| path.canonicalize().unwrap_or(path))
        })
        .collect::<Vec<_>>();
    matches.sort();
    matches.dedup();
    let path = match matches.as_slice() {
        [path] => path.clone(),
        [] => {
            return Err(MagError::Eval(format!(
                "cannot find module {name} in search roots"
            )))
        }
        paths => {
            return Err(MagError::Eval(format!(
                "module {name} is ambiguous across search roots: {}",
                paths
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )))
        }
    };
    env.profile_elapsed(Phase::ModuleResolve, resolve_started);
    let read_started = env.profile_started();
    let content = std::fs::read_to_string(&path)
        .map_err(|e| MagError::Eval(format!("cannot read module {name}: {e}")))?;
    env.profile_elapsed(Phase::ModuleRead, read_started);
    let lex_started = env.profile_started();
    let source_snapshot = crate::diagnostic::SourceSnapshot::file(&path, &content);
    let tokens = crate::lexer::tokenize_source(&source_snapshot)?;
    env.profile_elapsed(Phase::ModuleLex, lex_started);
    let parse_started = env.profile_started();
    let exprs = crate::parser::parse_source(&tokens, &source_snapshot)?;
    env.profile_elapsed(Phase::ModuleParse, parse_started);
    let mut module = env.module_env(name);
    let eval_started = env.profile_started();
    let result = eval_program(&mut module, &exprs);
    env.profile_elapsed(Phase::ModuleEvaluate, eval_started);
    match result {
        Ok(_) => {
            let defs = module.user_defs();
            env.finish_module(name, defs.clone());
            for (module_name, module_defs) in env.loaded_modules() {
                env.install_module(&module_name, module_defs);
            }
            Ok(Value::Map(std::sync::Arc::new(module_value_map(&defs))))
        }
        Err(e) => Err(e),
    }
}

fn module_value_map(defs: &BTreeMap<String, Vec<Value>>) -> BTreeMap<String, Value> {
    defs.iter()
        .filter_map(|(name, values)| values.first().cloned().map(|value| (name.clone(), value)))
        .collect()
}

pub(crate) fn resolve_workspace_path(root: &Path, relative: &str) -> Result<PathBuf, MagError> {
    let p = Path::new(relative);
    if p.is_absolute() || p.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(MagError::Eval(format!(
            "path escapes workspace: {relative}"
        )));
    }
    let joined = root.join(p);
    let canonical_root = root.canonicalize().unwrap_or_else(|_| root.into());
    if let Some(parent) = joined.parent() {
        let canonical_parent = parent.canonicalize().unwrap_or_else(|_| parent.into());
        if !canonical_parent.starts_with(canonical_root) {
            return Err(MagError::Eval(format!(
                "path escapes workspace: {relative}"
            )));
        }
    }
    Ok(joined)
}

// `require` needs its raw module name rather than an evaluated module symbol.
fn maybe_require(env: &mut Env, items: &[Expr]) -> Option<Result<Value, MagError>> {
    if matches!(items.first(),Some(Expr::Symbol(s)) if s=="require") {
        Some(if items.len() != 2 {
            Err(MagError::Arity {
                expected: 1,
                got: items.len() - 1,
            })
        } else {
            match &items[1] {
                Expr::Str(s) => eval_require(env, s),
                _ => Err(MagError::Eval("require expects a module string".into())),
            }
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn constant_function(env: &Env, name: &str, closure: Vec<(String, Value)>) -> Value {
        let mut captured = env.child_for_call();
        for (name, value) in closure {
            captured.define(&name, value);
        }
        let closure = captured.snapshot();
        Value::Fn(std::sync::Arc::new(FnValue {
            name: None,
            type_params: vec![],
            params: vec![],
            param_types: vec![],
            return_type: MagType::Int,
            body: vec![Expr::Symbol(name.into())],
            checked: None,
            closure,
        }))
    }

    fn typed_int(value: Value) -> i64 {
        match value {
            Value::Typed(value, _) => match value.as_ref() {
                Value::Int(value) => *value,
                other => panic!("expected typed Int, got {other:?}"),
            },
            other => panic!("expected typed Int, got {other:?}"),
        }
    }

    #[test]
    fn default_evaluation_budget_allows_exactly_1_000_000_steps() {
        let limit = crate::CompilerLimits::default().evaluation_steps;
        assert_eq!(limit, 1_000_000);
        let _fuel = fuel::install(limit);

        for _ in 0..limit {
            fuel::step().unwrap();
        }

        assert_eq!(fuel::remaining(), Some(0));
        assert!(matches!(fuel::step(), Err(MagError::Budget(_))));
    }

    #[test]
    fn lexical_closure_overrides_colliding_caller_binding() {
        let mut caller = Env::new();
        caller.define("value", Value::Int(99));
        let function = constant_function(&caller, "value", vec![("value".into(), Value::Int(1))]);
        let _fuel = fuel::install(100);

        assert_eq!(typed_int(apply(&caller, &function, &[]).unwrap()), 1);
    }

    #[test]
    fn caller_only_late_binding_is_invisible() {
        let mut caller = Env::new();
        caller.define("late", Value::Int(42));
        let function = constant_function(&caller, "late", vec![]);
        let _fuel = fuel::install(100);

        assert!(matches!(
            apply(&caller, &function, &[]),
            Err(MagError::Unresolved(name)) if name == "late"
        ));
    }

    #[test]
    fn unchecked_same_type_closure_duplicates_are_ambiguous() {
        let caller = Env::new();
        let function = constant_function(
            &caller,
            "value",
            vec![
                ("value".into(), Value::Int(1)),
                ("value".into(), Value::Int(2)),
            ],
        );
        let _fuel = fuel::install(100);

        assert!(apply(&caller, &function, &[])
            .unwrap_err()
            .to_string()
            .contains("ambiguous overload value"));
    }

    #[test]
    fn value_parameters_override_same_named_generic_bindings() {
        let function = Value::Fn(std::sync::Arc::new(FnValue {
            name: None,
            type_params: vec!["T".into()],
            params: vec!["T".into()],
            param_types: vec![MagType::Var("T".into())],
            return_type: MagType::Var("T".into()),
            body: vec![Expr::Symbol("T".into())],
            checked: None,
            closure: vec![],
        }));
        let _fuel = fuel::install(100);

        assert_eq!(
            typed_int(apply(&Env::new(), &function, &[Value::Int(7)]).unwrap()),
            7
        );
    }

    #[test]
    fn memoized_result_is_independent_of_caller_only_bindings() {
        let mut first_caller = Env::new();
        let function = constant_function(
            &first_caller,
            "value",
            vec![("value".into(), Value::Int(1))],
        );
        first_caller.define("value", Value::Int(99));
        let mut second_caller = first_caller.child_for_call();
        second_caller.define("value", Value::Int(100));
        let _fuel = fuel::install(100);

        assert_eq!(typed_int(apply(&first_caller, &function, &[]).unwrap()), 1);
        assert_eq!(typed_int(apply(&second_caller, &function, &[]).unwrap()), 1);
    }

    #[test]
    fn repeated_call_reuses_the_cached_shared_result_without_spending_fuel() {
        let source = r#"
            (let values [1 2 3])
            (let copy (fn [[items (List Int)]] -> (List Int)
              (map (fn [[item Int]] -> Int item) items)))
            (artifact {})
        "#;
        let expressions = crate::parser::parse(&crate::lexer::tokenize(source).unwrap()).unwrap();
        let mut env = Env::new();
        let _fuel = fuel::install(1_000);
        eval_program(&mut env, &expressions).unwrap();
        let argument = env.lookup("values").unwrap().clone();

        let first = apply_named(&env, "copy", argument.clone()).unwrap();
        let after_first = fuel::remaining().unwrap();
        let second = apply_named(&env, "copy", argument).unwrap();

        assert_eq!(fuel::remaining(), Some(after_first));
        match (first, second) {
            (Value::Typed(left, _), Value::Typed(right, _)) => {
                assert!(std::sync::Arc::ptr_eq(&left, &right));
            }
            values => panic!("expected typed results, got {values:?}"),
        }
    }
}
