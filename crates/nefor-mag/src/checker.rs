use crate::ast::{
    BindingId, CheckedBinding, CheckedBlock, CheckedExpr, CheckedExprKind, CheckedFn,
    CheckedMatchArm, CheckedParam, Expr, Value,
};
use crate::env::Env;
use crate::error::MagError;
use crate::types::MagType;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

type Locals = HashMap<String, Vec<MagType>>;

fn add_local(locals: &mut Locals, name: impl Into<String>, ty: MagType) -> Result<(), MagError> {
    let name = name.into();
    let overloads = locals.entry(name.clone()).or_default();
    if overloads.contains(&ty) {
        return Err(MagError::Type(format!(
            "duplicate visible overload {name}: {ty}"
        )));
    }
    overloads.push(ty);
    Ok(())
}

pub fn check_function_with_binding(
    env: &Env,
    binding: Option<&str>,
    params: &[String],
    param_types: &[MagType],
    result: &MagType,
    body: &[Expr],
) -> Result<(), MagError> {
    let mut locals = Locals::new();
    for (name, ty) in params.iter().cloned().zip(param_types.iter().cloned()) {
        add_local(&mut locals, name, ty)?;
    }
    if let Some(name) = binding {
        add_local(
            &mut locals,
            name.to_string(),
            MagType::Function(param_types.to_vec(), Box::new(result.clone())),
        )?;
    }
    for (name, ty) in params.iter().zip(param_types) {
        if env
            .lookup_candidates(name)
            .iter()
            .filter_map(value_type)
            .any(|visible| visible == *ty)
        {
            return Err(MagError::Type(format!(
                "duplicate visible overload {name}: {ty}"
            )));
        }
    }

    for expr in body {
        let Some((name, initializer)) = direct_let(expr)? else {
            continue;
        };
        if is_fn(initializer) {
            let ty = infer_fn_signature(env, &locals, initializer)?;
            add_local(&mut locals, name, ty)?;
        }
    }
    for expr in body {
        let Some((name, initializer)) = direct_let(expr)? else {
            continue;
        };
        if !is_fn(initializer) {
            let ty = infer(env, &mut locals, initializer)?;
            add_local(&mut locals, name, ty)?;
        }
    }
    let mut actual = MagType::Unit;
    for expr in body {
        if direct_let(expr)?.is_some() {
            if let Some((_, initializer)) = direct_let(expr)? {
                if is_fn(initializer) {
                    let _ = infer(env, &mut locals, initializer)?;
                }
            }
            continue;
        }
        actual = infer(env, &mut locals, expr).map_err(|error| match binding {
            Some(name) => MagError::Type(format!("in function {name}: {error}")),
            None => error,
        })?;
    }
    compatible(env, &actual, result, &mut HashMap::new()).map_err(|message| {
        MagError::Type(format!(
            "function {}returns {actual}, declared {result}: {message}",
            binding.map(|name| format!("{name} ")).unwrap_or_default()
        ))
    })
}

fn direct_let(expr: &Expr) -> Result<Option<(&str, &Expr)>, MagError> {
    let Expr::List(items) = expr else {
        return Ok(None);
    };
    if !matches!(items.first(), Some(Expr::Symbol(head)) if head == "let") {
        return Ok(None);
    }
    if items.len() != 3 {
        return Err(MagError::Type("let expects a name and value".into()));
    }
    let name = items[1]
        .as_symbol()
        .ok_or_else(|| MagError::Type("let name must be a symbol".into()))?;
    Ok(Some((name, &items[2])))
}

fn is_fn(expr: &Expr) -> bool {
    matches!(expr, Expr::List(items) if matches!(items.first(), Some(Expr::Symbol(head)) if head == "fn"))
}

fn function_type_params(expression: &Expr) -> Result<Vec<String>, MagError> {
    let Expr::List(items) = expression else {
        return Ok(vec![]);
    };
    let args = &items[1..];
    let Some(Expr::Vector(parameters)) = args.first() else {
        return Ok(vec![]);
    };
    if !matches!(args.get(1), Some(Expr::Vector(_))) {
        return Ok(vec![]);
    }
    parameters
        .iter()
        .map(|parameter| {
            parameter
                .as_symbol()
                .map(str::to_owned)
                .ok_or_else(|| MagError::Type("generic binder must be a symbol".into()))
        })
        .collect()
}

fn infer_fn_signature(env: &Env, outer: &Locals, expression: &Expr) -> Result<MagType, MagError> {
    let mut vars = HashSet::new();
    for ty in outer.values().flatten() {
        collect_vars(ty, &mut vars);
    }
    infer_fn_signature_scoped(env, &vars, expression)
}

fn infer_fn_signature_scoped(
    env: &Env,
    outer_vars: &HashSet<String>,
    expression: &Expr,
) -> Result<MagType, MagError> {
    let Expr::List(items) = expression else {
        unreachable!()
    };
    let args = &items[1..];
    if args.len() < 4 {
        return Err(MagError::Type("typed fn signature required".into()));
    }
    let empty = Expr::Vector(vec![]);
    let (type_expr, param_expr, arrow, return_expr) =
        if matches!(args.get(1), Some(Expr::Vector(_))) {
            (&args[0], &args[1], &args[2], &args[3])
        } else {
            (&empty, &args[0], &args[1], &args[2])
        };
    if !matches!(arrow, Expr::Symbol(symbol) if symbol == "->") {
        return Err(MagError::Type("fn signature requires ->".into()));
    }
    let mut vars = outer_vars.clone();
    let Expr::Vector(type_params) = type_expr else {
        unreachable!()
    };
    for parameter in type_params {
        vars.insert(
            parameter
                .as_symbol()
                .ok_or_else(|| MagError::Type("generic binder must be a symbol".into()))?
                .to_owned(),
        );
    }
    let Expr::Vector(params) = param_expr else {
        return Err(MagError::Type("fn parameters must be a vector".into()));
    };
    let param_types = params
        .iter()
        .map(|pair| match pair {
            Expr::Vector(values) if values.len() == 2 => {
                crate::eval::parse_type(env, &values[1], &vars)
            }
            _ => Err(MagError::Type("parameter must be [name Type]".into())),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let result = crate::eval::parse_type(env, return_expr, &vars)?;
    Ok(MagType::Function(param_types, Box::new(result)))
}

pub fn check_call(
    env: &Env,
    function: &crate::ast::FnValue,
    args: &[Value],
) -> Result<(MagType, HashMap<String, MagType>), MagError> {
    if function.param_types.len() != args.len() {
        return Err(MagError::Arity {
            expected: function.param_types.len(),
            got: args.len(),
        });
    }
    let actual_types = args
        .iter()
        .map(|value| {
            value_type(value).ok_or_else(|| {
                MagError::Type(format!(
                    "cannot pass {} as a typed argument",
                    value.type_name()
                ))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut subst = HashMap::new();
    let mut order = (0..function.param_types.len()).collect::<Vec<_>>();
    order.sort_by_key(|index| contains_union(&function.param_types[*index]));
    for index in order {
        let actual = &actual_types[index];
        let expected = substitute(&function.param_types[index], &subst);
        compatible(env, actual, &expected, &mut subst).map_err(MagError::Type)?;
    }
    Ok((substitute(&function.return_type, &subst), subst))
}

pub fn check_resolved_call(
    env: &Env,
    function: &crate::ast::FnValue,
    resolved: &MagType,
) -> Result<(MagType, HashMap<String, MagType>), MagError> {
    let MagType::Function(resolved_params, result) = resolved else {
        return Err(MagError::Type(format!(
            "checked call target must be a function, got {resolved}"
        )));
    };
    if resolved_params.len() != function.param_types.len() {
        return Err(MagError::Type(format!(
            "checked call arity changed from {} to {}",
            function.param_types.len(),
            resolved_params.len()
        )));
    }
    let mut substitution = HashMap::new();
    let mut order = (0..function.param_types.len()).collect::<Vec<_>>();
    order.sort_by_key(|index| contains_union(&function.param_types[*index]));
    for index in order {
        let expected = substitute(&function.param_types[index], &substitution);
        compatible(env, &resolved_params[index], &expected, &mut substitution)
            .map_err(MagError::Type)?;
    }
    let expected_result = substitute(&function.return_type, &substitution);
    compatible(env, result, &expected_result, &mut substitution).map_err(MagError::Type)?;
    Ok((result.as_ref().clone(), substitution))
}

fn infer(env: &Env, locals: &mut Locals, expr: &Expr) -> Result<MagType, MagError> {
    match expr {
        Expr::Nil => Ok(MagType::Unit),
        Expr::Bool(_) => Ok(MagType::Bool),
        Expr::Int(_) => Ok(MagType::Int),
        Expr::Float(_) => Ok(MagType::Float),
        Expr::Str(_) => Ok(MagType::String),
        Expr::Keyword(_) => Ok(MagType::String),
        Expr::Symbol(name) => match locals.get(name).map(Vec::as_slice) {
            Some([ty]) => Ok(ty.clone()),
            Some(types) => {
                let data = types
                    .iter()
                    .filter(|ty| !matches!(ty, MagType::Function(_, _)))
                    .collect::<Vec<_>>();
                match data.as_slice() {
                    [ty] => Ok((*ty).clone()),
                    _ => Err(MagError::Type(format!(
                        "ambiguous overload {name}; candidates: {}",
                        types
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))),
                }
            }
            None => env
                .lookup(name)
                .ok()
                .and_then(|value| value_type(&value))
                .ok_or_else(|| MagError::Unresolved(name.clone())),
        },
        Expr::Vector(xs) => infer_list(env, locals, xs),
        Expr::Map(fields) => {
            let mut out = BTreeMap::new();
            for (k, v) in fields {
                let key = match k {
                    Expr::Keyword(s) | Expr::Symbol(s) | Expr::Str(s) => s.clone(),
                    _ => {
                        return Err(MagError::Type(
                            "record key must be a keyword, symbol, or string".into(),
                        ))
                    }
                };
                out.insert(key, infer(env, locals, v)?);
            }
            Ok(MagType::Record(out))
        }
        Expr::List(items) => infer_form(env, locals, items),
    }
}

fn infer_list(env: &Env, locals: &mut Locals, xs: &[Expr]) -> Result<MagType, MagError> {
    if xs.is_empty() {
        return Ok(MagType::EmptyList);
    }
    let first = infer(env, locals, &xs[0])?;
    for item in &xs[1..] {
        let ty = infer(env, locals, item)?;
        compatible(env, &ty, &first, &mut HashMap::new()).map_err(MagError::Type)?;
    }
    Ok(MagType::List(Box::new(first)))
}

fn infer_form(env: &Env, locals: &mut Locals, items: &[Expr]) -> Result<MagType, MagError> {
    if items.is_empty() {
        return Ok(MagType::Unit);
    }
    if let Some(head) = items[0].as_symbol() {
        match head {
            "if" => {
                if items.len() < 3 || items.len() > 4 {
                    return Err(MagError::Type("if expects 2-3 arguments".into()));
                }
                compatible(
                    env,
                    &infer(env, locals, &items[1])?,
                    &MagType::Bool,
                    &mut HashMap::new(),
                )
                .map_err(MagError::Type)?;
                let left = infer(env, locals, &items[2])?;
                let right = if items.len() == 4 {
                    infer(env, locals, &items[3])?
                } else {
                    MagType::Unit
                };
                if compatible(env, &left, &right, &mut HashMap::new()).is_ok() {
                    return Ok(right);
                }
                return Ok(MagType::Union(vec![left, right]));
            }
            "match" => return infer_match(env, locals, items),
            "let" => {
                return Err(MagError::Type(
                    "let is only valid directly in a source or function block".into(),
                ));
            }
            "as" => {
                if items.len() != 3 {
                    return Err(MagError::Type("as expects a type and value".into()));
                }
                let mut vars = HashSet::new();
                for ty in locals.values().flatten() {
                    collect_vars(ty, &mut vars);
                }
                let target = crate::eval::parse_type(env, &items[1], &vars)?;
                let _source = match &items[2] {
                    Expr::Symbol(name) if locals.get(name).is_some_and(|types| types.len() > 1) => {
                        let matches = locals[name]
                            .iter()
                            .filter(|ty| compatible(env, ty, &target, &mut HashMap::new()).is_ok())
                            .cloned()
                            .collect::<Vec<_>>();
                        match matches.as_slice() {
                            [ty] => ty.clone(),
                            [] => {
                                return Err(MagError::Type(format!(
                                    "no overload {name} matches {target}"
                                )))
                            }
                            _ => {
                                return Err(MagError::Type(format!(
                                    "ambiguous overload {name} for {target}"
                                )))
                            }
                        }
                    }
                    Expr::Symbol(name) if env.lookup_candidates(name).len() > 1 => {
                        value_type(&env.lookup_by_type(name, &target)?)
                            .ok_or_else(|| MagError::Type(format!("{name} has no value type")))?
                    }
                    _ => target.clone(),
                };
                return Ok(target);
            }
            "type-tag" => {
                if items.len() != 2 {
                    return Err(MagError::Type("type-tag expects one type".into()));
                }
                let mut vars = HashSet::new();
                for ty in locals.values().flatten() {
                    collect_vars(ty, &mut vars);
                }
                return Ok(MagType::TypeTag(Box::new(crate::eval::parse_type(
                    env, &items[1], &vars,
                )?)));
            }
            "fn" => return infer_fn(env, locals, items),
            _ => {}
        }
    }
    if let Some(name) = items[0].as_symbol() {
        let env_candidates = env.lookup_candidates(name);
        let builtin = env_candidates
            .iter()
            .any(|candidate| matches!(candidate, Value::BuiltinFn(_)));
        let mut signatures = locals
            .get(name)
            .into_iter()
            .flatten()
            .filter_map(|candidate| match candidate {
                MagType::Function(params, result) => {
                    let mut variables = HashSet::new();
                    collect_vars(candidate, &mut variables);
                    Some((variables.is_empty(), params.clone(), (**result).clone()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        for signature in env_candidates.iter().filter_map(|candidate| {
            let Value::Fn(function) = candidate else {
                return None;
            };
            Some((
                function.type_params.is_empty(),
                function.param_types.clone(),
                function.return_type.clone(),
            ))
        }) {
            if !signatures.contains(&signature) {
                signatures.push(signature);
            }
        }
        if !signatures.is_empty() {
            let argument_types = items[1..]
                .iter()
                .map(|argument| infer(env, locals, argument))
                .collect::<Result<Vec<_>, _>>()?;
            let mut matching = signatures
                .iter()
                .filter_map(|(concrete, params, result)| {
                    if params.len() != argument_types.len() {
                        return None;
                    }
                    let mut substitution = HashMap::new();
                    let mut order = (0..params.len()).collect::<Vec<_>>();
                    order.sort_by_key(|index| contains_union(&params[*index]));
                    for index in order {
                        let actual = &argument_types[index];
                        let expected = substitute(&params[index], &substitution);
                        compatible(env, actual, &expected, &mut substitution).ok()?;
                    }
                    Some((*concrete, substitute(result, &substitution)))
                })
                .collect::<Vec<_>>();
            if matching.len() > 1 {
                let concrete = matching
                    .iter()
                    .filter(|(concrete, _)| *concrete)
                    .cloned()
                    .collect::<Vec<_>>();
                if concrete.len() == 1 {
                    matching = concrete;
                }
            }
            return match matching.as_slice() {
                [(_, result)] => Ok(result.clone()),
                [] if builtin => infer_builtin(env, locals, name, &items[1..]),
                [] => Err(MagError::Type(format!("no overload {name} matches call"))),
                _ => Err(MagError::Type(format!(
                    "ambiguous overload {name} for call"
                ))),
            };
        } else if builtin {
            return infer_builtin(env, locals, name, &items[1..]);
        }
    }
    let callable = infer(env, locals, &items[0])?;
    let (params, result) = match callable {
        MagType::Function(p, r) => (p, r),
        other => return Err(MagError::Type(format!("cannot call {other}"))),
    };
    if params.len() != items.len() - 1 {
        return Err(MagError::Type(format!(
            "call expects {} arguments, got {}",
            params.len(),
            items.len() - 1
        )));
    }
    let mut subst = HashMap::new();
    let argument_types = items[1..]
        .iter()
        .map(|argument| infer(env, locals, argument))
        .collect::<Result<Vec<_>, _>>()?;
    let mut order = (0..params.len()).collect::<Vec<_>>();
    order.sort_by_key(|index| contains_union(&params[*index]));
    for index in order {
        let actual = &argument_types[index];
        let expected = substitute(&params[index], &subst);
        compatible(env, actual, &expected, &mut subst).map_err(MagError::Type)?;
    }
    Ok(substitute(&result, &subst))
}

fn infer_match(env: &Env, locals: &Locals, items: &[Expr]) -> Result<MagType, MagError> {
    let (value, arms) = match_form(items)?;
    let value_type = infer(env, &mut locals.clone(), value)?;
    let constructors = sum_constructors(env, &value_type)?;
    let mut seen = HashSet::new();
    let mut result = None;
    for arm in arms {
        let (constructor_expression, binding, body) = match_arm(arm)?;
        let mut vars = HashSet::new();
        for ty in locals.values().flatten() {
            collect_vars(ty, &mut vars);
        }
        let constructor_type = crate::eval::parse_type(env, constructor_expression, &vars)?;
        let concrete = nominal_constructor(env, &constructor_type)?;
        if !constructors.contains(&concrete) {
            return Err(MagError::Type(format!(
                "constructor {constructor_type} is not an arm of {value_type}"
            )));
        }
        if !seen.insert(concrete) {
            return Err(MagError::Type(format!(
                "duplicate match arm for {constructor_type}"
            )));
        }
        let mut arm_locals = locals.clone();
        add_local(&mut arm_locals, binding.to_owned(), constructor_type)?;
        let body_type = infer(env, &mut arm_locals, body)?;
        if let Some(current) = &result {
            compatible(env, &body_type, current, &mut HashMap::new()).map_err(|_| {
                MagError::Type(format!(
                    "match arms must return one compatible type, got {current} and {body_type}"
                ))
            })?;
        } else {
            result = Some(body_type);
        }
    }
    ensure_exhaustive(&constructors, &seen)?;
    Ok(result.unwrap_or(MagType::Unit))
}

fn infer_fn(env: &Env, outer: &Locals, items: &[Expr]) -> Result<MagType, MagError> {
    let args = &items[1..];
    if args.len() < 4 {
        return Err(MagError::Type("typed fn signature required".into()));
    }
    let empty = Expr::Vector(vec![]);
    let (type_expr, param_expr, arrow, return_expr, body) =
        if matches!(args.get(1), Some(Expr::Vector(_))) {
            (&args[0], &args[1], &args[2], &args[3], &args[4..])
        } else {
            (&empty, &args[0], &args[1], &args[2], &args[3..])
        };
    if !matches!(arrow,Expr::Symbol(s) if s=="->") {
        return Err(MagError::Type("fn signature requires ->".into()));
    }
    let mut vars = HashSet::new();
    for ty in outer.values().flatten() {
        collect_vars(ty, &mut vars);
    }
    let declared_vars = match type_expr {
        Expr::Vector(xs) => xs
            .iter()
            .map(|x| {
                x.as_symbol()
                    .map(str::to_owned)
                    .ok_or_else(|| MagError::Type("generic binder must be a symbol".into()))
            })
            .collect::<Result<HashSet<_>, _>>()?,
        _ => unreachable!(),
    };
    vars.extend(declared_vars);
    let pairs = match param_expr {
        Expr::Vector(v) => v,
        _ => return Err(MagError::Type("fn parameters must be a vector".into())),
    };
    let mut names = Vec::<String>::new();
    let mut types = vec![];
    for pair in pairs {
        match pair {
            Expr::Vector(v) if v.len() == 2 => {
                names.push(
                    v[0].as_symbol()
                        .ok_or_else(|| MagError::Type("parameter name must be a symbol".into()))?
                        .into(),
                );
                types.push(crate::eval::parse_type(env, &v[1], &vars)?);
            }
            _ => return Err(MagError::Type("parameter must be [name Type]".into())),
        }
    }
    let result = crate::eval::parse_type(env, return_expr, &vars)?;
    let mut locals = outer.clone();
    for (name, ty) in names.iter().cloned().zip(types.iter().cloned()) {
        add_local(&mut locals, name, ty)?;
    }
    for expr in body {
        let Some((name, initializer)) = direct_let(expr)? else {
            continue;
        };
        if is_fn(initializer) {
            let ty = infer_fn_signature(env, &locals, initializer)?;
            add_local(&mut locals, name, ty)?;
        }
    }
    for expr in body {
        let Some((name, initializer)) = direct_let(expr)? else {
            continue;
        };
        if !is_fn(initializer) {
            let ty = infer(env, &mut locals, initializer)?;
            add_local(&mut locals, name, ty)?;
        }
    }
    let mut actual = MagType::Unit;
    for expr in body {
        if let Some((_, initializer)) = direct_let(expr)? {
            if is_fn(initializer) {
                let _ = infer(env, &mut locals, initializer)?;
            }
            continue;
        }
        actual = infer(env, &mut locals, expr)?;
    }
    compatible(env, &actual, &result, &mut HashMap::new()).map_err(|message| {
        MagError::Type(format!(
            "nested function returns {actual}, declared {result}: {message}"
        ))
    })?;
    Ok(MagType::Function(types, Box::new(result)))
}

fn infer_builtin(
    env: &Env,
    locals: &mut Locals,
    name: &str,
    args: &[Expr],
) -> Result<MagType, MagError> {
    let exact = |expected| {
        if args.len() == expected {
            Ok(())
        } else {
            Err(MagError::Type(format!(
                "{name} expects {expected} arguments, got {}",
                args.len()
            )))
        }
    };
    match name {
        "get" => {
            exact(2)?;
            let target = infer(env, locals, &args[0])?;
            if matches!(target, MagType::HostInputs) {
                return Err(MagError::Type("get expects a map".into()));
            }
            let key_type = infer(env, locals, &args[1])?;
            compatible(env, &key_type, &MagType::String, &mut HashMap::new())
                .map_err(MagError::Type)?;
            let key = match &args[1] {
                Expr::Str(s) | Expr::Keyword(s) => Some(s.as_str()),
                _ => None,
            };
            field_type(env, &target, key)
                .ok_or_else(|| MagError::Type(format!("cannot get {:?} from {target}", key)))
        }
        "assoc" => {
            exact(3)?;
            let target = infer(env, locals, &args[0])?;
            let key_type = infer(env, locals, &args[1])?;
            compatible(env, &key_type, &MagType::String, &mut HashMap::new())
                .map_err(MagError::Type)?;
            let key = match &args[1] {
                Expr::Str(s) | Expr::Keyword(s) => Some(s.as_str()),
                _ => None,
            };
            let expected = field_type(env, &target, key)
                .ok_or_else(|| MagError::Type(format!("cannot assoc {:?} into {target}", key)))?;
            let value = infer(env, locals, &args[2])?;
            compatible(env, &value, &expected, &mut HashMap::new()).map_err(MagError::Type)?;
            Ok(target)
        }
        "count" => {
            exact(1)?;
            match infer(env, locals, &args[0])? {
                MagType::List(_) | MagType::Map(_, _) | MagType::Record(_) | MagType::String => {
                    Ok(MagType::Int)
                }
                actual => Err(MagError::Type(format!(
                    "count expects a collection, got {actual}"
                ))),
            }
        }
        "=" => {
            exact(2)?;
            let left = infer(env, locals, &args[0])?;
            let right = infer(env, locals, &args[1])?;
            compatible(env, &left, &right, &mut HashMap::new()).map_err(MagError::Type)?;
            Ok(MagType::Bool)
        }
        "not" => {
            exact(1)?;
            let actual = infer(env, locals, &args[0])?;
            compatible(env, &actual, &MagType::Bool, &mut HashMap::new())
                .map_err(MagError::Type)?;
            Ok(MagType::Bool)
        }
        "host-input" => {
            exact(2)?;
            let key = infer(env, locals, &args[0])?;
            compatible(env, &key, &MagType::String, &mut HashMap::new()).map_err(MagError::Type)?;
            match infer(env, locals, &args[1])? {
                MagType::TypeTag(expected) => Ok(*expected),
                actual => Err(MagError::Type(format!(
                    "host-input expects TypeTag, got {actual}"
                ))),
            }
        }
        "type-evidence" => {
            exact(1)?;
            match infer(env, locals, &args[0])? {
                MagType::TypeTag(_) => Ok(MagType::TypeDescriptor),
                actual => Err(MagError::Type(format!(
                    "type-evidence expects TypeTag, got {actual}"
                ))),
            }
        }
        "type-schema" => {
            exact(1)?;
            match infer(env, locals, &args[0])? {
                MagType::TypeTag(_) => Ok(MagType::TypeSchema),
                actual => Err(MagError::Type(format!(
                    "type-schema expects TypeTag, got {actual}"
                ))),
            }
        }
        "type-id" => {
            exact(1)?;
            let descriptor = infer(env, locals, &args[0])?;
            compatible(
                env,
                &descriptor,
                &MagType::TypeDescriptor,
                &mut HashMap::new(),
            )
            .map_err(MagError::Type)?;
            Ok(MagType::SemanticTypeId)
        }
        "value-type-id" => {
            exact(2)?;
            let actual = infer(env, locals, &args[0])?;
            match infer(env, locals, &args[1])? {
                MagType::TypeTag(expected) => {
                    compatible(env, &expected, &actual, &mut HashMap::new())
                        .map_err(MagError::Type)?;
                }
                MagType::TypeDescriptor => {}
                _ => {
                    return Err(MagError::Type(
                        "value-type-id expects TypeTag or TypeDescriptor evidence".into(),
                    ))
                }
            }
            Ok(MagType::SemanticTypeId)
        }
        "value-type-evidence" => {
            exact(2)?;
            let _ = infer(env, locals, &args[0])?;
            let evidence = infer(env, locals, &args[1])?;
            compatible(
                env,
                &evidence,
                &MagType::TypeDescriptor,
                &mut HashMap::new(),
            )
            .map_err(MagError::Type)?;
            Ok(MagType::TypeDescriptor)
        }
        "or" => {
            exact(2)?;
            let a = infer(env, locals, &args[0])?;
            let b = infer(env, locals, &args[1])?;
            if compatible(env, &a, &b, &mut HashMap::new()).is_ok() {
                Ok(b)
            } else {
                Ok(MagType::Union(vec![a, b]))
            }
        }
        "str" => Ok(MagType::String),
        "canonical" => {
            exact(1)?;
            let _ = infer(env, locals, &args[0])?;
            Ok(MagType::String)
        }
        "function-name" => {
            exact(1)?;
            match infer(env, locals, &args[0])? {
                MagType::Function(_, _) => Ok(MagType::String),
                actual => Err(MagError::Type(format!(
                    "function-name expects a function, got {actual}"
                ))),
            }
        }
        "conforms?" => {
            exact(2)?;
            let _ = infer(env, locals, &args[0])?;
            let evidence = infer(env, locals, &args[1])?;
            compatible(
                env,
                &evidence,
                &MagType::TypeDescriptor,
                &mut HashMap::new(),
            )
            .map_err(MagError::Type)?;
            Ok(MagType::Bool)
        }
        "fail" => {
            exact(1)?;
            let _ = infer(env, locals, &args[0])?;
            Ok(MagType::Never)
        }
        "pack" => {
            exact(1)?;
            let _ = infer(env, locals, &args[0])?;
            Ok(MagType::PackedValue)
        }
        "packed-empty-record?" => {
            exact(1)?;
            let value = infer(env, locals, &args[0])?;
            compatible(env, &value, &MagType::PackedValue, &mut HashMap::new())
                .map_err(MagError::Type)?;
            Ok(MagType::Bool)
        }
        "packed-record-has-only-key?"
        | "packed-record-has-only-keys?"
        | "packed-field-conforms?" => {
            exact(if name == "packed-field-conforms?" {
                3
            } else {
                2
            })?;
            let value = infer(env, locals, &args[0])?;
            compatible(env, &value, &MagType::PackedValue, &mut HashMap::new())
                .map_err(MagError::Type)?;
            let key = infer(env, locals, &args[1])?;
            let expected_key = if name == "packed-record-has-only-keys?" {
                MagType::List(Box::new(MagType::String))
            } else {
                MagType::String
            };
            compatible(env, &key, &expected_key, &mut HashMap::new()).map_err(MagError::Type)?;
            if name == "packed-field-conforms?" {
                let descriptor = infer(env, locals, &args[2])?;
                compatible(
                    env,
                    &descriptor,
                    &MagType::TypeDescriptor,
                    &mut HashMap::new(),
                )
                .map_err(MagError::Type)?;
            }
            Ok(MagType::Bool)
        }
        "descriptor-accepts?"
        | "descriptor-accepts-value?"
        | "descriptor-input-covered-by?"
        | "descriptor-input-assignments"
        | "descriptor-output-covered-by?" => {
            exact(2)?;
            let descriptor = infer(env, locals, &args[0])?;
            compatible(
                env,
                &descriptor,
                &MagType::TypeDescriptor,
                &mut HashMap::new(),
            )
            .map_err(MagError::Type)?;
            let expected = if matches!(name, "descriptor-accepts?" | "descriptor-accepts-value?") {
                MagType::TypeDescriptor
            } else {
                MagType::List(Box::new(MagType::TypeDescriptor))
            };
            let value = infer(env, locals, &args[1])?;
            compatible(env, &value, &expected, &mut HashMap::new()).map_err(MagError::Type)?;
            if name == "descriptor-input-assignments" {
                Ok(MagType::List(Box::new(MagType::Int)))
            } else {
                Ok(MagType::Bool)
            }
        }
        "descriptor-table" => {
            exact(1)?;
            let descriptors = infer(env, locals, &args[0])?;
            compatible(
                env,
                &descriptors,
                &MagType::List(Box::new(MagType::TypeDescriptor)),
                &mut HashMap::new(),
            )
            .map_err(MagError::Type)?;
            Ok(MagType::Map(
                Box::new(MagType::String),
                Box::new(MagType::TypeDescriptor),
            ))
        }
        "read" => {
            if !(1..=2).contains(&args.len()) {
                return Err(MagError::Type(format!(
                    "read expects 1-2 arguments, got {}",
                    args.len()
                )));
            }
            let path = infer(env, locals, &args[0])?;
            compatible(env, &path, &MagType::String, &mut HashMap::new())
                .map_err(MagError::Type)?;
            if args.len() == 2 {
                let _ = infer(env, locals, &args[1])?;
            }
            Ok(MagType::String)
        }
        "read-json" => {
            exact(1)?;
            let path = infer(env, locals, &args[0])?;
            compatible(env, &path, &MagType::String, &mut HashMap::new())
                .map_err(MagError::Type)?;
            Ok(MagType::JsonValue)
        }
        "artifact" => {
            exact(1)?;
            let _ = infer(env, locals, &args[0])?;
            Ok(MagType::Artifact)
        }
        "strip-margin" => {
            exact(1)?;
            let value = infer(env, locals, &args[0])?;
            compatible(env, &value, &MagType::String, &mut HashMap::new())
                .map_err(MagError::Type)?;
            Ok(MagType::String)
        }
        "replace" => {
            exact(3)?;
            for argument in args {
                let value = infer(env, locals, argument)?;
                compatible(env, &value, &MagType::String, &mut HashMap::new())
                    .map_err(MagError::Type)?;
            }
            Ok(MagType::String)
        }
        "concat" => {
            exact(2)?;
            let a = infer(env, locals, &args[0])?;
            let b = infer(env, locals, &args[1])?;
            compatible(env, &a, &b, &mut HashMap::new()).map_err(MagError::Type)?;
            Ok(a)
        }
        "remove-at" => {
            exact(2)?;
            let collection = infer(env, locals, &args[0])?;
            let index = infer(env, locals, &args[1])?;
            compatible(env, &index, &MagType::Int, &mut HashMap::new()).map_err(MagError::Type)?;
            match collection {
                MagType::List(_) => Ok(collection),
                actual => Err(MagError::Type(format!(
                    "remove-at expects List, got {actual}"
                ))),
            }
        }
        "keys" => {
            exact(1)?;
            match infer(env, locals, &args[0])? {
                MagType::Map(_, _) | MagType::Record(_) => {
                    Ok(MagType::List(Box::new(MagType::String)))
                }
                actual => Err(MagError::Type(format!("keys expects a map, got {actual}"))),
            }
        }
        "first" => {
            exact(1)?;
            match infer(env, locals, &args[0])? {
                MagType::List(item) => Ok(*item),
                actual => Err(MagError::Type(format!("first expects List, got {actual}"))),
            }
        }
        "map" | "filter" | "flat-map" | "sort-by" | "group-by" => {
            exact(2)?;
            let fun = infer(env, locals, &args[0])?;
            let collection = infer(env, locals, &args[1])?;
            let item = match collection {
                MagType::List(t) => *t,
                _ => return Err(MagError::Type(format!("{name} expects List"))),
            };
            let (params, result) = match fun {
                MagType::Function(p, r) => (p, r),
                _ => return Err(MagError::Type(format!("{name} expects function"))),
            };
            if params.len() != 1 {
                return Err(MagError::Type(format!(
                    "{name} callback expects 1 parameter"
                )));
            }
            compatible(env, &item, &params[0], &mut HashMap::new()).map_err(MagError::Type)?;
            if name == "filter" {
                compatible(env, &result, &MagType::Bool, &mut HashMap::new())
                    .map_err(MagError::Type)?;
                Ok(MagType::List(Box::new(item)))
            } else if name == "sort-by" || name == "group-by" {
                compatible(env, &result, &MagType::String, &mut HashMap::new())
                    .map_err(MagError::Type)?;
                if name == "group-by" {
                    Ok(MagType::Map(
                        Box::new(MagType::String),
                        Box::new(MagType::List(Box::new(item))),
                    ))
                } else {
                    Ok(MagType::List(Box::new(item)))
                }
            } else if name == "flat-map" {
                match *result {
                    MagType::List(_) => Ok(*result),
                    actual => Err(MagError::Type(format!(
                        "flat-map callback must return List, got {actual}"
                    ))),
                }
            } else {
                Ok(MagType::List(result))
            }
        }
        "indexed-map" => {
            exact(2)?;
            let fun = infer(env, locals, &args[0])?;
            let collection = infer(env, locals, &args[1])?;
            let item = match collection {
                MagType::List(t) => *t,
                _ => return Err(MagError::Type("indexed-map expects List".into())),
            };
            let (params, result) = match fun {
                MagType::Function(p, r) => (p, r),
                _ => return Err(MagError::Type("indexed-map expects function".into())),
            };
            if params.len() != 2 {
                return Err(MagError::Type(
                    "indexed-map callback expects 2 parameters".into(),
                ));
            }
            compatible(env, &MagType::Int, &params[0], &mut HashMap::new())
                .map_err(MagError::Type)?;
            compatible(env, &item, &params[1], &mut HashMap::new()).map_err(MagError::Type)?;
            Ok(MagType::List(result))
        }
        "fold" => {
            exact(3)?;
            let fun = infer(env, locals, &args[0])?;
            let init = infer(env, locals, &args[1])?;
            let collection = infer(env, locals, &args[2])?;
            let item = match collection {
                MagType::List(t) => *t,
                _ => return Err(MagError::Type("fold expects List".into())),
            };
            let (params, result) = match fun {
                MagType::Function(p, r) => (p, r),
                _ => return Err(MagError::Type("fold expects function".into())),
            };
            if params.len() != 2 {
                return Err(MagError::Type("fold callback expects 2 parameters".into()));
            }
            compatible(env, &init, &params[0], &mut HashMap::new()).map_err(MagError::Type)?;
            compatible(env, &item, &params[1], &mut HashMap::new()).map_err(MagError::Type)?;
            compatible(env, &result, &params[0], &mut HashMap::new()).map_err(MagError::Type)?;
            Ok(*result)
        }
        _ => Err(MagError::Type(format!("no type rule for builtin {name}"))),
    }
}

#[derive(Clone)]
struct CheckedCandidate {
    id: BindingId,
    ty: MagType,
    generic_binders: Vec<String>,
    contributes_type_vars: bool,
}

type CheckedScope = HashMap<String, Vec<CheckedCandidate>>;
const TYPE_BINDER_SCOPE_KEY: &str = "\0type-binders";

pub(crate) const BUILTIN_NAMES: &[&str] = &[
    "str",
    "strip-margin",
    "replace",
    "map",
    "group-by",
    "indexed-map",
    "filter",
    "flat-map",
    "fold",
    "concat",
    "get",
    "assoc",
    "keys",
    "count",
    "first",
    "canonical",
    "function-name",
    "sort-by",
    "remove-at",
    "conforms?",
    "or",
    "not",
    "=",
    "fail",
    "type-evidence",
    "read",
    "read-json",
    "require",
    "artifact",
    "type-schema",
    "type-id",
    "value-type-id",
    "value-type-evidence",
    "pack",
    "packed-empty-record?",
    "packed-record-has-only-key?",
    "packed-record-has-only-keys?",
    "packed-field-conforms?",
    "descriptor-accepts?",
    "descriptor-accepts-value?",
    "descriptor-input-covered-by?",
    "descriptor-input-assignments",
    "descriptor-output-covered-by?",
    "descriptor-table",
    "host-input",
];

fn builtin_overload_types(name: &str, candidate: Option<&MagType>) -> Vec<MagType> {
    let var = |name: &str| MagType::Var(format!("\0builtin.{name}"));
    let function =
        |params: Vec<MagType>, result: MagType| MagType::Function(params, Box::new(result));
    let list = |item: MagType| MagType::List(Box::new(item));
    let map = |key: MagType, value: MagType| MagType::Map(Box::new(key), Box::new(value));
    let tag = |value: MagType| MagType::TypeTag(Box::new(value));
    let descriptor = MagType::TypeDescriptor;
    let packed = MagType::PackedValue;
    let mut signatures = match name {
        "str" => candidate
            .and_then(|candidate| match candidate {
                MagType::Function(params, _) => {
                    Some(vec![function(params.clone(), MagType::String)])
                }
                _ => None,
            })
            .unwrap_or_default(),
        "strip-margin" => vec![function(vec![MagType::String], MagType::String)],
        "replace" => vec![function(
            vec![MagType::String, MagType::String, MagType::String],
            MagType::String,
        )],
        "count" => vec![
            function(vec![list(var("item"))], MagType::Int),
            function(vec![MagType::String], MagType::Int),
            function(vec![map(var("key"), var("value"))], MagType::Int),
        ],
        "first" => vec![function(vec![list(var("item"))], var("item"))],
        "remove-at" => vec![function(
            vec![list(var("item")), MagType::Int],
            list(var("item")),
        )],
        "concat" => vec![
            function(
                vec![list(var("item")), list(var("item"))],
                list(var("item")),
            ),
            function(vec![MagType::String, MagType::String], MagType::String),
        ],
        "not" => vec![function(vec![MagType::Bool], MagType::Bool)],
        "=" => vec![function(vec![var("value"), var("value")], MagType::Bool)],
        "host-input" => vec![function(
            vec![MagType::String, tag(var("value"))],
            var("value"),
        )],
        "type-evidence" => vec![function(vec![tag(var("value"))], descriptor)],
        "type-schema" => vec![function(vec![tag(var("value"))], MagType::TypeSchema)],
        "type-id" => vec![function(vec![descriptor], MagType::SemanticTypeId)],
        "value-type-id" => vec![
            function(
                vec![var("value"), tag(var("value"))],
                MagType::SemanticTypeId,
            ),
            function(
                vec![var("value"), descriptor.clone()],
                MagType::SemanticTypeId,
            ),
        ],
        "value-type-evidence" => vec![function(
            vec![var("value"), descriptor.clone()],
            descriptor.clone(),
        )],
        "canonical" => vec![function(vec![var("value")], MagType::String)],
        "function-name" => vec![function(
            vec![function(vec![var("input")], var("output"))],
            MagType::String,
        )],
        "conforms?" => vec![function(vec![var("value"), descriptor], MagType::Bool)],
        "fail" => vec![function(vec![var("value")], MagType::Never)],
        "pack" => vec![function(vec![var("value")], packed.clone())],
        "packed-empty-record?" => vec![function(vec![packed.clone()], MagType::Bool)],
        "packed-record-has-only-key?" => vec![function(
            vec![packed.clone(), MagType::String],
            MagType::Bool,
        )],
        "packed-record-has-only-keys?" => vec![function(
            vec![packed.clone(), list(MagType::String)],
            MagType::Bool,
        )],
        "packed-field-conforms?" => vec![function(
            vec![packed, MagType::String, descriptor],
            MagType::Bool,
        )],
        "descriptor-accepts?" | "descriptor-accepts-value?" => vec![function(
            vec![descriptor.clone(), descriptor.clone()],
            MagType::Bool,
        )],
        "descriptor-input-covered-by?" | "descriptor-output-covered-by?" => vec![function(
            vec![descriptor.clone(), list(descriptor.clone())],
            MagType::Bool,
        )],
        "descriptor-input-assignments" => vec![function(
            vec![descriptor.clone(), list(descriptor.clone())],
            list(MagType::Int),
        )],
        "descriptor-table" => vec![function(
            vec![list(descriptor.clone())],
            map(MagType::String, descriptor.clone()),
        )],
        "read" => vec![
            function(vec![MagType::String], MagType::String),
            function(vec![MagType::String, var("fallback")], MagType::String),
        ],
        "read-json" => vec![function(vec![MagType::String], MagType::JsonValue)],
        "require" => vec![function(vec![MagType::String], MagType::Unit)],
        "artifact" => vec![function(vec![var("value")], MagType::Artifact)],
        "or" => vec![function(vec![var("value"), var("value")], var("value"))],
        "keys" => vec![function(
            vec![map(var("key"), var("value"))],
            list(MagType::String),
        )],
        "get" => vec![function(
            vec![map(var("key"), var("value")), MagType::String],
            var("value"),
        )],
        "assoc" => vec![function(
            vec![map(var("key"), var("value")), MagType::String, var("value")],
            map(var("key"), var("value")),
        )],
        "map" => vec![function(
            vec![
                function(vec![var("item")], var("result")),
                list(var("item")),
            ],
            list(var("result")),
        )],
        "group-by" => vec![function(
            vec![
                function(vec![var("item")], MagType::String),
                list(var("item")),
            ],
            map(MagType::String, list(var("item"))),
        )],
        "filter" => vec![function(
            vec![
                function(vec![var("item")], MagType::Bool),
                list(var("item")),
            ],
            list(var("item")),
        )],
        "flat-map" => vec![function(
            vec![
                function(vec![var("item")], list(var("result"))),
                list(var("item")),
            ],
            list(var("result")),
        )],
        "sort-by" => vec![function(
            vec![
                function(vec![var("item")], MagType::String),
                list(var("item")),
            ],
            list(var("item")),
        )],
        "indexed-map" => vec![function(
            vec![
                function(vec![MagType::Int, var("item")], var("result")),
                list(var("item")),
            ],
            list(var("result")),
        )],
        "fold" => vec![function(
            vec![
                function(vec![var("state"), var("item")], var("state")),
                var("state"),
                list(var("item")),
            ],
            var("state"),
        )],
        _ => Vec::new(),
    };
    if let Some(MagType::Function(params, _)) = candidate {
        if name == "or" && params.len() == 2 {
            signatures.push(function(params.clone(), MagType::Union(params.clone())));
        }
        if let Some(MagType::Record(fields)) = params.first() {
            match name {
                "count" => signatures.push(function(vec![params[0].clone()], MagType::Int)),
                "keys" => signatures.push(function(vec![params[0].clone()], list(MagType::String))),
                "get" if params.len() == 2 && params[1] == MagType::String => {
                    signatures.extend(
                        fields
                            .values()
                            .cloned()
                            .map(|field| function(vec![params[0].clone(), MagType::String], field)),
                    );
                }
                "assoc" if params.len() == 3 && params[1] == MagType::String => {
                    signatures.extend(fields.values().cloned().map(|field| {
                        function(
                            vec![params[0].clone(), MagType::String, field],
                            params[0].clone(),
                        )
                    }));
                }
                _ => {}
            }
        }
    }
    signatures
}

fn collides_with_builtin(env: &Env, name: &str, candidate: &MagType) -> bool {
    builtin_overload_types(name, Some(candidate))
        .iter()
        .any(|builtin| {
            let bindable = internal_type_variables(builtin);
            compatible_with_bindable(env, candidate, builtin, &mut HashMap::new(), &bindable)
                .is_ok()
        })
}

/// Resolves a source block into typed expressions whose authored references
/// point at stable binding identities. Evaluation never has to repeat name or
/// overload resolution.
pub fn compile_block(env: &Env, expressions: &[Expr]) -> Result<CheckedBlock, MagError> {
    compile_block_in(env, &[], expressions, None)
}

fn compile_block_in(
    env: &Env,
    outer: &[CheckedScope],
    expressions: &[Expr],
    result_expected: Option<&MagType>,
) -> Result<CheckedBlock, MagError> {
    let mut declarations = Vec::new();
    for expression in expressions {
        if let Some(declaration) = direct_let(expression)? {
            declarations.push(declaration);
        }
    }
    let allocated = declarations
        .iter()
        .map(|(name, initializer)| (*name, *initializer, env.allocate_binding_id(name, None)))
        .collect::<Vec<_>>();
    let declared_names = allocated
        .iter()
        .map(|(name, _, _)| *name)
        .collect::<HashSet<_>>();

    let mut current = CheckedScope::new();
    let scoped_type_vars = visible_type_variables(outer);
    let mut pending = Vec::new();
    for (name, initializer, id) in &allocated {
        if is_fn(initializer) {
            let ty = infer_fn_signature_scoped(env, &scoped_type_vars, initializer)?;
            insert_checked_candidate(
                env,
                outer,
                &mut current,
                name,
                CheckedCandidate {
                    id: *id,
                    ty,
                    generic_binders: function_type_params(initializer)?,
                    contributes_type_vars: false,
                },
            )?;
        } else {
            pending.push((*name, *initializer, *id));
        }
    }

    // Strict initializers are inferred as a dependency fixpoint. Function
    // bodies are intentionally not visited here: their signatures provide the
    // complete peer inventory before any body is checked.
    while !pending.is_empty() {
        let mut next = Vec::new();
        let mut progressed = false;
        for (name, initializer, id) in pending {
            let mut types = visible_types(env, outer, &current);
            match infer_shape(env, &mut types, &scoped_type_vars, initializer) {
                Ok(ty) => {
                    insert_checked_candidate(
                        env,
                        outer,
                        &mut current,
                        name,
                        CheckedCandidate {
                            id,
                            ty,
                            generic_binders: vec![],
                            contributes_type_vars: false,
                        },
                    )?;
                    progressed = true;
                }
                Err(MagError::Unresolved(symbol)) if declared_names.contains(symbol.as_str()) => {
                    next.push((name, initializer, id));
                }
                Err(error @ MagError::Unresolved(_)) => return Err(error),
                Err(error) => return Err(error),
            }
        }
        if !progressed {
            let names = next.iter().map(|(name, _, _)| *name).collect::<Vec<_>>();
            return Err(MagError::Type(format!(
                "cannot infer recursive strict bindings: {}",
                names.join(", ")
            )));
        }
        pending = next;
    }

    let mut scopes = outer.to_vec();
    scopes.push(current.clone());
    let mut bindings = Vec::with_capacity(allocated.len());
    for (name, initializer, id) in allocated {
        let candidate = current
            .get(name)
            .and_then(|candidates| candidates.iter().find(|candidate| candidate.id == id))
            .ok_or_else(|| MagError::Unresolved(name.into()))?;
        let checked = if is_fn(initializer) {
            compile_function(env, &scopes, Some(name), initializer, Some(&candidate.ty))?
        } else {
            compile_expr(env, &scopes, initializer, Some(&candidate.ty))?
        };
        bindings.push(CheckedBinding {
            id,
            name: name.into(),
            ty: candidate.ty.clone(),
            initializer: Arc::new(checked),
        });
    }

    let last_expression = expressions
        .iter()
        .rposition(|expression| direct_let(expression).ok().flatten().is_none());
    let mut checked_expressions = Vec::new();
    for (index, expression) in expressions.iter().enumerate() {
        if direct_let(expression)?.is_none() {
            let expected = (Some(index) == last_expression)
                .then_some(result_expected)
                .flatten();
            let checked = match compile_expr(env, &scopes, expression, expected) {
                Ok(checked) => checked,
                Err(_) if expected.is_some() => compile_expr(env, &scopes, expression, None)?,
                Err(error) => return Err(error),
            };
            checked_expressions.push(checked);
        }
    }
    env.profile_counters(|counters| {
        counters.checked_bindings = counters
            .checked_bindings
            .saturating_add(bindings.len() as u64);
    });
    Ok(CheckedBlock {
        frame_layout: bindings.iter().map(|binding| binding.id).collect(),
        bindings,
        expressions: checked_expressions,
    })
}

fn insert_checked_candidate(
    env: &Env,
    outer: &[CheckedScope],
    current: &mut CheckedScope,
    name: &str,
    candidate: CheckedCandidate,
) -> Result<(), MagError> {
    let canonical = canonical_type(&candidate.ty);
    if visible_candidates(env, outer, current, name)
        .iter()
        .any(|candidate| canonical_type(&candidate.ty) == canonical)
        || collides_with_builtin(env, name, &candidate.ty)
    {
        return Err(MagError::Type(format!(
            "duplicate visible overload {name}: {}",
            candidate.ty
        )));
    }
    env.set_binding_type(candidate.id, candidate.ty.clone());
    current.entry(name.to_owned()).or_default().push(candidate);
    Ok(())
}

fn env_candidates(env: &Env, name: &str) -> Vec<CheckedCandidate> {
    env.lookup_candidate_ids(name)
        .into_iter()
        .filter_map(|id| {
            env.binding_metadata(id)
                .and_then(|metadata| metadata.ty)
                .map(|ty| {
                    let generic_binders = match env.ready_binding(id) {
                        Ok(Value::Fn(function)) => function.type_params.clone(),
                        _ => vec![],
                    };
                    CheckedCandidate {
                        id,
                        ty,
                        generic_binders,
                        contributes_type_vars: false,
                    }
                })
        })
        .collect()
}

fn builtin_id(env: &Env, name: &str) -> Option<BindingId> {
    env.lookup_candidate_ids(name).into_iter().find(
        |id| matches!(env.ready_binding(*id), Ok(Value::BuiltinFn(ref builtin)) if builtin == name),
    )
}

fn visible_candidates(
    env: &Env,
    scopes: &[CheckedScope],
    current: &CheckedScope,
    name: &str,
) -> Vec<CheckedCandidate> {
    let mut candidates = scopes
        .iter()
        .flat_map(|scope| scope.get(name).into_iter().flatten().cloned())
        .chain(current.get(name).into_iter().flatten().cloned())
        .chain(env_candidates(env, name))
        .collect::<Vec<_>>();
    let mut seen = HashSet::new();
    candidates.retain(|candidate| seen.insert(candidate.id));
    candidates
}

fn all_candidates(env: &Env, scopes: &[CheckedScope], name: &str) -> Vec<CheckedCandidate> {
    visible_candidates(env, scopes, &CheckedScope::new(), name)
}

fn visible_types(_env: &Env, scopes: &[CheckedScope], current: &CheckedScope) -> Locals {
    let mut types = Locals::new();
    for scope in scopes.iter().chain(std::iter::once(current)) {
        for (name, candidates) in scope {
            let entry = types.entry(name.clone()).or_default();
            for candidate in candidates {
                if !entry.contains(&candidate.ty) {
                    entry.push(candidate.ty.clone());
                }
            }
        }
    }
    types
}

fn infer_shape(
    env: &Env,
    locals: &mut Locals,
    scoped_type_vars: &HashSet<String>,
    expression: &Expr,
) -> Result<MagType, MagError> {
    match expression {
        Expr::List(items) if matches!(items.first(), Some(Expr::Symbol(head)) if head == "fn") => {
            infer_fn_signature_scoped(env, scoped_type_vars, expression)
        }
        Expr::Vector(items) => {
            if items.is_empty() {
                return Ok(MagType::EmptyList);
            }
            let first = infer_shape(env, locals, scoped_type_vars, &items[0])?;
            for item in &items[1..] {
                let actual = infer_shape(env, locals, scoped_type_vars, item)?;
                compatible_static(env, &actual, &first, &mut HashMap::new())
                    .map_err(MagError::Type)?;
            }
            Ok(MagType::List(Box::new(first)))
        }
        Expr::Map(fields) => fields
            .iter()
            .map(|(key, value)| {
                let key = record_key(key)?;
                Ok((key, infer_shape(env, locals, scoped_type_vars, value)?))
            })
            .collect::<Result<BTreeMap<_, _>, MagError>>()
            .map(MagType::Record),
        Expr::List(items) if matches!(items.first(), Some(Expr::Symbol(head)) if head == "as") => {
            if items.len() != 3 {
                return Err(MagError::Type("as expects a type and value".into()));
            }
            crate::eval::parse_type(env, &items[1], scoped_type_vars)
        }
        Expr::List(items) if matches!(items.first(), Some(Expr::Symbol(head)) if head == "type-tag") =>
        {
            if items.len() != 2 {
                return Err(MagError::Type("type-tag expects one type".into()));
            }
            Ok(MagType::TypeTag(Box::new(crate::eval::parse_type(
                env,
                &items[1],
                scoped_type_vars,
            )?)))
        }
        Expr::List(items) if matches!(items.first(), Some(Expr::Symbol(head)) if head == "if") => {
            if !(3..=4).contains(&items.len()) {
                return Err(MagError::Type("if expects 2-3 arguments".into()));
            }
            let condition = infer_shape(env, locals, scoped_type_vars, &items[1])?;
            compatible_static(env, &condition, &MagType::Bool, &mut HashMap::new())
                .map_err(MagError::Type)?;
            let left = infer_shape(env, locals, scoped_type_vars, &items[2])?;
            let right = if items.len() == 4 {
                infer_shape(env, locals, scoped_type_vars, &items[3])?
            } else {
                MagType::Unit
            };
            if compatible_static(env, &left, &right, &mut HashMap::new()).is_ok() {
                Ok(right)
            } else {
                Ok(MagType::Union(vec![left, right]))
            }
        }
        _ => infer(env, locals, expression),
    }
}

fn compile_expr(
    env: &Env,
    scopes: &[CheckedScope],
    expression: &Expr,
    expected: Option<&MagType>,
) -> Result<CheckedExpr, MagError> {
    let checked = match expression {
        Expr::Nil => checked(MagType::Unit, CheckedExprKind::Unit),
        Expr::Bool(value) => checked(MagType::Bool, CheckedExprKind::Bool(*value)),
        Expr::Int(value) => checked(MagType::Int, CheckedExprKind::Int(*value)),
        Expr::Float(value) => checked(MagType::Float, CheckedExprKind::Float(*value)),
        Expr::Str(value) => checked(MagType::String, CheckedExprKind::Str(value.clone())),
        Expr::Keyword(value) => checked(MagType::String, CheckedExprKind::Keyword(value.clone())),
        Expr::Symbol(name) => compile_symbol(env, scopes, name, expected)?,
        Expr::Vector(items) => compile_vector(env, scopes, items, expected)?,
        Expr::Map(fields) => compile_map(env, scopes, fields, expected)?,
        Expr::List(items) => compile_form(env, scopes, items, expected)?,
    };
    if let Some(expected) = expected {
        compatible_static(env, &checked.ty, expected, &mut HashMap::new())
            .map_err(MagError::Type)?;
    }
    env.profile_counters(|counters| {
        counters.checked_expressions = counters.checked_expressions.saturating_add(1);
    });
    Ok(checked)
}

fn checked(ty: MagType, kind: CheckedExprKind) -> CheckedExpr {
    CheckedExpr { ty, kind }
}

fn compile_symbol(
    env: &Env,
    scopes: &[CheckedScope],
    name: &str,
    expected: Option<&MagType>,
) -> Result<CheckedExpr, MagError> {
    let candidates = all_candidates(env, scopes, name);
    let mut matches = candidates
        .iter()
        .filter_map(|candidate| {
            let (candidate_type, mut bindable) = instantiate_candidate(candidate);
            let resolved = if let Some(expected) = expected {
                bindable.extend(internal_type_variables(expected));
                let mut substitution = HashMap::new();
                if !candidate.generic_binders.is_empty() {
                    compatible_with_bindable(
                        env,
                        expected,
                        &candidate_type,
                        &mut substitution,
                        &bindable,
                    )
                    .ok()?;
                    substitute(&candidate_type, &substitution)
                } else {
                    compatible_with_bindable(
                        env,
                        &candidate_type,
                        expected,
                        &mut substitution,
                        &bindable,
                    )
                    .ok()?;
                    candidate_type
                }
            } else {
                candidate_type
            };
            Some((candidate, resolved))
        })
        .collect::<Vec<_>>();
    if matches.len() > 1 {
        let concrete = matches
            .iter()
            .filter(|(candidate, _)| candidate.generic_binders.is_empty())
            .count();
        if concrete == 1 {
            matches.retain(|(candidate, _)| candidate.generic_binders.is_empty());
        }
    }
    if expected.is_none() && matches.len() > 1 {
        let data = matches
            .iter()
            .filter(|(_, resolved)| !matches!(resolved, MagType::Function(_, _)))
            .cloned()
            .collect::<Vec<_>>();
        if data.len() == 1 {
            matches = data;
        }
    }
    match matches.as_slice() {
        [(candidate, resolved)] => Ok(checked(
            resolved.clone(),
            CheckedExprKind::BindingRef(candidate.id),
        )),
        [] if candidates.is_empty() => Err(MagError::Unresolved(name.into())),
        [] => Err(MagError::Type(format!(
            "no overload {name} matches {}",
            expected
                .map(ToString::to_string)
                .unwrap_or_else(|| "context".into())
        ))),
        _ => Err(MagError::Type(format!(
            "ambiguous overload {name}; candidates: {}",
            matches
                .iter()
                .map(|(_, resolved)| resolved.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

fn compile_vector(
    env: &Env,
    scopes: &[CheckedScope],
    items: &[Expr],
    expected: Option<&MagType>,
) -> Result<CheckedExpr, MagError> {
    if let Some(MagType::Product(_)) = expected {
        let checked_items = items
            .iter()
            .map(|item| compile_expr(env, scopes, item, None))
            .collect::<Result<Vec<_>, _>>()?;
        let actual = checked_items
            .iter()
            .map(|item| item.ty.clone())
            .collect::<Vec<_>>();
        return Ok(checked(
            MagType::Product(actual),
            CheckedExprKind::Vector(checked_items),
        ));
    }
    let expected_item = match expected {
        Some(MagType::List(item)) => Some(item.as_ref()),
        _ => None,
    };
    let mut checked_items = Vec::with_capacity(items.len());
    let mut item_type = expected_item.cloned();
    for item in items {
        let value = compile_expr(env, scopes, item, expected_item)?;
        if let Some(ty) = expected_item {
            compatible_static(env, &value.ty, ty, &mut HashMap::new()).map_err(MagError::Type)?;
        } else if let Some(current) = &item_type {
            if compatible_static(env, &value.ty, current, &mut HashMap::new()).is_err() {
                let mut alternatives = match current {
                    MagType::Union(alternatives) => alternatives.clone(),
                    ty => vec![ty.clone()],
                };
                if !alternatives.contains(&value.ty) {
                    alternatives.push(value.ty.clone());
                }
                item_type = Some(MagType::Union(alternatives));
            }
        } else {
            item_type = Some(value.ty.clone());
        }
        checked_items.push(value);
    }
    let ty = item_type
        .map(|item| MagType::List(Box::new(item)))
        .unwrap_or(MagType::EmptyList);
    Ok(checked(ty, CheckedExprKind::Vector(checked_items)))
}

fn record_key(expression: &Expr) -> Result<String, MagError> {
    match expression {
        Expr::Keyword(key) | Expr::Symbol(key) | Expr::Str(key) => Ok(key.clone()),
        _ => Err(MagError::Type(
            "record key must be a keyword, symbol, or string".into(),
        )),
    }
}

fn compile_map(
    env: &Env,
    scopes: &[CheckedScope],
    fields: &[(Expr, Expr)],
    expected: Option<&MagType>,
) -> Result<CheckedExpr, MagError> {
    let mut checked_fields = Vec::with_capacity(fields.len());
    let mut field_types = BTreeMap::new();
    for (key, value) in fields {
        let key = record_key(key)?;
        let expected_field = match expected {
            Some(MagType::Record(fields)) => fields.get(&key),
            Some(MagType::Map(_, value)) => Some(value.as_ref()),
            _ => None,
        };
        let value = compile_expr(env, scopes, value, expected_field)?;
        field_types.insert(key.clone(), value.ty.clone());
        checked_fields.push((key, value));
    }
    Ok(checked(
        MagType::Record(field_types),
        CheckedExprKind::Map(checked_fields),
    ))
}

fn compile_form(
    env: &Env,
    scopes: &[CheckedScope],
    items: &[Expr],
    expected: Option<&MagType>,
) -> Result<CheckedExpr, MagError> {
    if items.is_empty() {
        return Ok(checked(MagType::Unit, CheckedExprKind::Unit));
    }
    if let Some(head) = items[0].as_symbol() {
        match head {
            "let" => {
                return Err(MagError::Type(
                    "let is only valid directly in a source or function block".into(),
                ))
            }
            "if" => return compile_if(env, scopes, items, expected),
            "match" => return compile_match(env, scopes, items, expected),
            "as" => return compile_ascribe(env, scopes, items),
            "type-tag" => return compile_type_tag(env, scopes, items),
            "fn" => {
                return compile_function(env, scopes, None, &Expr::List(items.to_vec()), expected)
            }
            _ => {}
        }
        let candidates = all_candidates(env, scopes, head);
        let function_candidates = candidates
            .into_iter()
            .filter(|candidate| matches!(candidate.ty, MagType::Function(_, _)))
            .collect::<Vec<_>>();
        if !function_candidates.is_empty() {
            let user_call = compile_overloaded_call(
                env,
                scopes,
                head,
                &function_candidates,
                &items[1..],
                expected,
            );
            if user_call.is_ok() || builtin_id(env, head).is_none() {
                return user_call;
            }
        }
        if let Some(id) = builtin_id(env, head) {
            return compile_builtin_call(env, scopes, head, id, &items[1..], expected);
        }
    }
    let callee = compile_expr(env, scopes, &items[0], None)?;
    let MagType::Function(params, result) = &callee.ty else {
        return Err(MagError::Type(format!("cannot call {}", callee.ty)));
    };
    if params.len() != items.len() - 1 {
        return Err(MagError::Arity {
            expected: params.len(),
            got: items.len() - 1,
        });
    }
    let mut substitution = HashMap::new();
    let mut args = Vec::with_capacity(params.len());
    for (expression, parameter) in items[1..].iter().zip(params) {
        let parameter = substitute(parameter, &substitution);
        let argument = compile_expr(env, scopes, expression, Some(&parameter))?;
        compatible_static(env, &argument.ty, &parameter, &mut substitution)
            .map_err(MagError::Type)?;
        args.push(argument);
    }
    let result = substitute(result, &substitution);
    if let Some(expected) = expected {
        let bindable = internal_type_variables(&callee.ty);
        constrain_result(env, &result, expected, &mut substitution, &bindable)?;
    }
    let result = substitute(&result, &substitution);
    Ok(checked(
        result,
        CheckedExprKind::Call {
            callee: Box::new(callee),
            args,
        },
    ))
}

fn compile_match(
    env: &Env,
    scopes: &[CheckedScope],
    items: &[Expr],
    expected: Option<&MagType>,
) -> Result<CheckedExpr, MagError> {
    let (value_expression, arm_expressions) = match_form(items)?;
    let value = compile_expr(env, scopes, value_expression, None)?;
    let constructors = sum_constructors(env, &value.ty)?;
    let mut seen = HashSet::new();
    let mut arms = Vec::with_capacity(arm_expressions.len());
    let mut result = expected.cloned();

    for arm_expression in arm_expressions {
        let (constructor_expression, binding_name, body_expression) = match_arm(arm_expression)?;
        let constructor_type = parse_checked_type(env, scopes, constructor_expression)?;
        let constructor = nominal_constructor(env, &constructor_type)?;
        if !constructors.contains(&constructor) {
            return Err(MagError::Type(format!(
                "constructor {constructor_type} is not an arm of {}",
                value.ty
            )));
        }
        if !seen.insert(constructor.clone()) {
            return Err(MagError::Type(format!(
                "duplicate match arm for {constructor_type}"
            )));
        }

        let binding_id = env.allocate_binding_id(binding_name, Some(constructor_type.clone()));
        let mut arm_scope = CheckedScope::new();
        insert_checked_candidate(
            env,
            scopes,
            &mut arm_scope,
            binding_name,
            CheckedCandidate {
                id: binding_id,
                ty: constructor_type.clone(),
                generic_binders: vec![],
                contributes_type_vars: false,
            },
        )?;
        let mut arm_scopes = scopes.to_vec();
        arm_scopes.push(arm_scope);
        let body = compile_expr(env, &arm_scopes, body_expression, result.as_ref())?;
        if let Some(current) = &result {
            compatible_static(env, &body.ty, current, &mut HashMap::new()).map_err(|_| {
                MagError::Type(format!(
                    "match arms must return one compatible type, got {current} and {}",
                    body.ty
                ))
            })?;
        } else {
            result = Some(body.ty.clone());
        }
        arms.push(CheckedMatchArm {
            constructor,
            binding: CheckedParam {
                id: binding_id,
                name: binding_name.to_owned(),
                ty: constructor_type,
            },
            body: Box::new(body),
        });
    }
    ensure_exhaustive(&constructors, &seen)?;
    Ok(checked(
        result.unwrap_or(MagType::Unit),
        CheckedExprKind::Match {
            value: Box::new(value),
            arms,
        },
    ))
}

fn match_form(items: &[Expr]) -> Result<(&Expr, &[Expr]), MagError> {
    if items.len() < 3 {
        return Err(MagError::Type(
            "match expects a value and at least one arm".into(),
        ));
    }
    Ok((&items[1], &items[2..]))
}

fn match_arm(arm: &Expr) -> Result<(&Expr, &str, &Expr), MagError> {
    let Expr::Vector(items) = arm else {
        return Err(MagError::Type(
            "match arm must be [Constructor binding expression]".into(),
        ));
    };
    let [constructor, binding, body] = items.as_slice() else {
        return Err(MagError::Type(
            "match arm must be [Constructor binding expression]".into(),
        ));
    };
    let binding = binding
        .as_symbol()
        .ok_or_else(|| MagError::Type("match binding must be a symbol".into()))?;
    Ok((constructor, binding, body))
}

fn sum_constructors(env: &Env, ty: &MagType) -> Result<Vec<MagType>, MagError> {
    if !is_sum_type(env, ty, &mut HashSet::new())? {
        return Err(MagError::Type(format!(
            "match expects a sum value, got {ty}"
        )));
    }
    let mut constructors = Vec::new();
    collect_sum_constructors(env, ty, &mut HashSet::new(), &mut constructors)?;
    let mut names = HashSet::new();
    for constructor in &constructors {
        let MagType::Named(name, _) = constructor else {
            unreachable!("sum constructor collection accepts only named arms")
        };
        if !names.insert(name.clone()) {
            return Err(MagError::Type(format!(
                "match cannot distinguish repeated nominal constructor {name}"
            )));
        }
    }
    Ok(constructors)
}

fn collect_sum_constructors(
    env: &Env,
    ty: &MagType,
    aliases: &mut HashSet<String>,
    constructors: &mut Vec<MagType>,
) -> Result<(), MagError> {
    match ty {
        MagType::Union(arms) => {
            for arm in arms {
                collect_sum_constructors(env, arm, aliases, constructors)?;
            }
        }
        MagType::Named(name, arguments) => {
            if is_sum_type(env, ty, &mut HashSet::new())? {
                let key = format!("{name}<{arguments:?}>");
                if !aliases.insert(key.clone()) {
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
                    .collect::<HashMap<_, _>>();
                let body = substitute(&declaration.body, &substitutions);
                collect_sum_constructors(env, &body, aliases, constructors)?;
                aliases.remove(&key);
            } else {
                constructors.push(ty.clone());
            }
        }
        other => {
            return Err(MagError::Type(format!(
                "match requires nominal constructor arms, found {other}"
            )))
        }
    }
    Ok(())
}

fn nominal_constructor(env: &Env, ty: &MagType) -> Result<MagType, MagError> {
    if !matches!(ty, MagType::Named(_, _)) {
        return Err(MagError::Type(format!(
            "match arm must name a nominal constructor, got {ty}"
        )));
    }
    if is_sum_type(env, ty, &mut HashSet::new())? {
        return Err(MagError::Type(format!(
            "match arm must name one constructor, got sum alias {ty}"
        )));
    }
    Ok(ty.clone())
}

fn ensure_exhaustive(constructors: &[MagType], seen: &HashSet<MagType>) -> Result<(), MagError> {
    let missing = constructors
        .iter()
        .filter(|constructor| !seen.contains(*constructor))
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(MagError::Type(format!(
            "non-exhaustive match; missing {}",
            missing.join(", ")
        )))
    }
}

fn is_sum_type(env: &Env, ty: &MagType, aliases: &mut HashSet<String>) -> Result<bool, MagError> {
    match ty {
        MagType::Union(_) => Ok(true),
        MagType::Named(name, arguments) => {
            let key = format!("{name}<{arguments:?}>");
            if !aliases.insert(key.clone()) {
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
                .collect::<HashMap<_, _>>();
            let body = substitute(&declaration.body, &substitutions);
            let result = is_sum_type(env, &body, aliases);
            aliases.remove(&key);
            result
        }
        _ => Ok(false),
    }
}

fn compile_if(
    env: &Env,
    scopes: &[CheckedScope],
    items: &[Expr],
    expected: Option<&MagType>,
) -> Result<CheckedExpr, MagError> {
    if !(3..=4).contains(&items.len()) {
        return Err(MagError::Type("if expects 2-3 arguments".into()));
    }
    let condition = compile_expr(env, scopes, &items[1], Some(&MagType::Bool))?;
    let then_branch = compile_expr(env, scopes, &items[2], expected)?;
    let else_branch = if items.len() == 4 {
        compile_expr(env, scopes, &items[3], expected)?
    } else {
        checked(MagType::Unit, CheckedExprKind::Unit)
    };
    let ty =
        if compatible_static(env, &then_branch.ty, &else_branch.ty, &mut HashMap::new()).is_ok() {
            else_branch.ty.clone()
        } else {
            MagType::Union(vec![then_branch.ty.clone(), else_branch.ty.clone()])
        };
    Ok(checked(
        ty,
        CheckedExprKind::If {
            condition: Box::new(condition),
            then_branch: Box::new(then_branch),
            else_branch: Box::new(else_branch),
        },
    ))
}

fn compile_ascribe(
    env: &Env,
    scopes: &[CheckedScope],
    items: &[Expr],
) -> Result<CheckedExpr, MagError> {
    if items.len() != 3 {
        return Err(MagError::Type("as expects a type and value".into()));
    }
    let target = parse_checked_type(env, scopes, &items[1])?;
    // `as` is the explicit nominal/sum refinement boundary. Structural
    // conformance and constructor-evidence diagnostics belong to evaluation,
    // so the checker must not reject the very conversion `as` authorizes.
    // Expected type is used only where it selects an overload or gives a
    // product literal its authored positional shape.
    let source_expected = match &items[2] {
        Expr::Symbol(name) if all_candidates(env, scopes, name).len() > 1 => Some(&target),
        _ => None,
    };
    let value = match &items[2] {
        Expr::Vector(values) if matches!(target, MagType::Product(_)) => {
            compile_vector(env, scopes, values, Some(&target))?
        }
        Expr::List(values) if !matches!(values.first(), Some(Expr::Symbol(head)) if matches!(head.as_str(), "if" | "as" | "type-tag" | "fn" | "let")) => {
            compile_form(env, scopes, values, Some(&target))?
        }
        source => compile_expr(env, scopes, source, source_expected)?,
    };
    Ok(checked(
        target.clone(),
        CheckedExprKind::Ascribe {
            target,
            value: Box::new(value),
        },
    ))
}

fn compile_type_tag(
    env: &Env,
    scopes: &[CheckedScope],
    items: &[Expr],
) -> Result<CheckedExpr, MagError> {
    if items.len() != 2 {
        return Err(MagError::Type("type-tag expects one type".into()));
    }
    let target = parse_checked_type(env, scopes, &items[1])?;
    Ok(checked(
        MagType::TypeTag(Box::new(target.clone())),
        CheckedExprKind::TypeTag(target),
    ))
}

fn compile_overloaded_call(
    env: &Env,
    scopes: &[CheckedScope],
    name: &str,
    candidates: &[CheckedCandidate],
    expressions: &[Expr],
    expected_result: Option<&MagType>,
) -> Result<CheckedExpr, MagError> {
    if let [candidate] = candidates {
        let (candidate_type, bindable) = instantiate_candidate(candidate);
        let MagType::Function(params, result) = &candidate_type else {
            unreachable!()
        };
        if params.len() != expressions.len() {
            return Err(MagError::Arity {
                expected: params.len(),
                got: expressions.len(),
            });
        }
        let (args, mut substitution) =
            compile_call_args(env, scopes, expressions, params, &bindable)?;
        let output = substitute(result, &substitution);
        if let Some(expected) = expected_result {
            constrain_result(env, &output, expected, &mut substitution, &bindable)?;
        }
        let output = substitute(result, &substitution);
        let callee_type = MagType::Function(
            params
                .iter()
                .map(|parameter| substitute(parameter, &substitution))
                .collect(),
            Box::new(output.clone()),
        );
        return Ok(checked(
            output,
            CheckedExprKind::Call {
                callee: Box::new(checked(
                    callee_type,
                    CheckedExprKind::BindingRef(candidate.id),
                )),
                args,
            },
        ));
    }
    let mut matches = Vec::new();
    for candidate in candidates {
        let (candidate_type, bindable) = instantiate_candidate(candidate);
        let MagType::Function(params, result) = &candidate_type else {
            continue;
        };
        if params.len() != expressions.len() {
            continue;
        }
        if let Ok((args, mut substitution)) =
            compile_call_args(env, scopes, expressions, params, &bindable)
        {
            let output = substitute(result, &substitution);
            if let Some(expected) = expected_result {
                if constrain_result(env, &output, expected, &mut substitution, &bindable).is_err() {
                    continue;
                }
            }
            let output = substitute(result, &substitution);
            let callee_type = MagType::Function(
                params
                    .iter()
                    .map(|parameter| substitute(parameter, &substitution))
                    .collect(),
                Box::new(output.clone()),
            );
            matches.push((candidate, args, output, callee_type));
        }
    }
    if matches.len() > 1 {
        let concrete = matches
            .iter()
            .filter(|(candidate, _, _, _)| candidate.generic_binders.is_empty())
            .count();
        if concrete == 1 {
            matches.retain(|(candidate, _, _, _)| candidate.generic_binders.is_empty());
        }
    }
    match matches.as_slice() {
        [(candidate, args, result, callee_type)] => {
            let callee = checked(
                callee_type.clone(),
                CheckedExprKind::BindingRef(candidate.id),
            );
            Ok(checked(
                result.clone(),
                CheckedExprKind::Call {
                    callee: Box::new(callee),
                    args: args.clone(),
                },
            ))
        }
        [] => Err(MagError::Type(format!("no overload {name} matches call"))),
        _ => Err(MagError::Type(format!(
            "ambiguous overload {name} for call"
        ))),
    }
}

fn constrain_result(
    env: &Env,
    output: &MagType,
    expected: &MagType,
    substitution: &mut HashMap<String, MagType>,
    bindable: &HashSet<String>,
) -> Result<(), MagError> {
    if has_type_variables(output) {
        compatible_with_bindable(env, expected, output, substitution, bindable)
            .map_err(MagError::Type)
    } else {
        compatible_with_bindable(env, output, expected, substitution, bindable)
            .map_err(MagError::Type)
    }
}

fn compile_call_args(
    env: &Env,
    scopes: &[CheckedScope],
    expressions: &[Expr],
    params: &[MagType],
    bindable: &HashSet<String>,
) -> Result<(Vec<CheckedExpr>, HashMap<String, MagType>), MagError> {
    let mut substitution = HashMap::new();
    let mut args = vec![None; params.len()];
    let mut order = (0..params.len()).collect::<Vec<_>>();
    order.sort_by_key(|index| contains_union(&params[*index]));
    for index in order {
        let expression = &expressions[index];
        let parameter = &params[index];
        let parameter = substitute(parameter, &substitution);
        let argument = compile_expr(env, scopes, expression, Some(&parameter))?;
        compatible_with_bindable(env, &argument.ty, &parameter, &mut substitution, bindable)
            .map_err(MagError::Type)?;
        args[index] = Some(argument);
    }
    let args = args
        .into_iter()
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| MagError::Type("internal error: unchecked call argument".into()))?;
    Ok((args, substitution))
}

fn compile_builtin_call(
    env: &Env,
    scopes: &[CheckedScope],
    name: &str,
    id: BindingId,
    expressions: &[Expr],
    expected: Option<&MagType>,
) -> Result<CheckedExpr, MagError> {
    if name == "get" {
        if expressions.len() != 2 {
            return Err(MagError::Arity {
                expected: 2,
                got: expressions.len(),
            });
        }
        let target = compile_expr(env, scopes, &expressions[0], None)?;
        if matches!(target.ty, MagType::HostInputs) {
            return Err(MagError::Type("get expects a map".into()));
        }
        let key = compile_expr(env, scopes, &expressions[1], Some(&MagType::String))?;
        let key_name = match &expressions[1] {
            Expr::Str(key) | Expr::Keyword(key) => Some(key.as_str()),
            _ => None,
        };
        let output = field_type(env, &target.ty, key_name)
            .ok_or_else(|| MagError::Type(format!("cannot get {key_name:?} from {}", target.ty)))?;
        return Ok(checked(
            output.clone(),
            CheckedExprKind::Call {
                callee: Box::new(checked(
                    MagType::Function(
                        vec![target.ty.clone(), MagType::String],
                        Box::new(output.clone()),
                    ),
                    CheckedExprKind::BindingRef(id),
                )),
                args: vec![target, key],
            },
        ));
    }
    if name == "canonical" && expressions.len() != 1 {
        return Err(MagError::Arity {
            expected: 1,
            got: expressions.len(),
        });
    }
    if name == "artifact" {
        if expressions.len() != 1 {
            return Err(MagError::Arity {
                expected: 1,
                got: expressions.len(),
            });
        }
        let data = compile_expr(env, scopes, &expressions[0], None)?;
        let result = MagType::Artifact;
        return Ok(checked(
            result.clone(),
            CheckedExprKind::Call {
                callee: Box::new(checked(
                    MagType::Function(vec![data.ty.clone()], Box::new(result.clone())),
                    CheckedExprKind::BindingRef(id),
                )),
                args: vec![data],
            },
        ));
    }
    if name == "str" {
        let args = expressions
            .iter()
            .map(|expression| compile_expr(env, scopes, expression, None))
            .collect::<Result<Vec<_>, _>>()?;
        let result = MagType::String;
        return Ok(checked(
            result.clone(),
            CheckedExprKind::Call {
                callee: Box::new(checked(
                    MagType::Function(
                        args.iter().map(|argument| argument.ty.clone()).collect(),
                        Box::new(result.clone()),
                    ),
                    CheckedExprKind::BindingRef(id),
                )),
                args,
            },
        ));
    }
    if matches!(
        name,
        "map" | "filter" | "flat-map" | "sort-by" | "group-by" | "indexed-map"
    ) {
        return compile_collection_builtin(env, scopes, name, id, expressions, expected);
    }
    if name == "fold" {
        return compile_fold_builtin(env, scopes, id, expressions);
    }
    let mut locals = visible_types(env, scopes, &CheckedScope::new());
    let result = infer_builtin(env, &mut locals, name, expressions)?;
    let args = expressions
        .iter()
        .map(|expression| compile_expr(env, scopes, expression, None))
        .collect::<Result<Vec<_>, _>>()?;
    let callee_type = MagType::Function(
        args.iter().map(|argument| argument.ty.clone()).collect(),
        Box::new(result.clone()),
    );
    Ok(checked(
        result,
        CheckedExprKind::Call {
            callee: Box::new(checked(callee_type, CheckedExprKind::BindingRef(id))),
            args,
        },
    ))
}

fn compile_collection_builtin(
    env: &Env,
    scopes: &[CheckedScope],
    name: &str,
    id: BindingId,
    expressions: &[Expr],
    expected: Option<&MagType>,
) -> Result<CheckedExpr, MagError> {
    if expressions.len() != 2 {
        return Err(MagError::Arity {
            expected: 2,
            got: expressions.len(),
        });
    }
    let collection = compile_expr(env, scopes, &expressions[1], None)?;
    let MagType::List(item) = &collection.ty else {
        return Err(MagError::Type(format!("{name} expects List")));
    };
    let callback_result = match name {
        "filter" => MagType::Bool,
        "sort-by" | "group-by" => MagType::String,
        "map" => match expected {
            Some(MagType::List(result)) => (**result).clone(),
            _ => MagType::Var("\0builtin.result".into()),
        },
        "flat-map" => expected
            .cloned()
            .unwrap_or_else(|| MagType::List(Box::new(MagType::Var("\0builtin.result".into())))),
        "indexed-map" => match expected {
            Some(MagType::List(result)) => (**result).clone(),
            _ => MagType::Var("\0builtin.result".into()),
        },
        _ => unreachable!(),
    };
    let callback_params = if name == "indexed-map" {
        vec![MagType::Int, (**item).clone()]
    } else {
        vec![(**item).clone()]
    };
    let callback_expected = MagType::Function(callback_params, Box::new(callback_result));
    let callback =
        compile_expr(env, scopes, &expressions[0], Some(&callback_expected)).map_err(|error| {
            match name {
                "sort-by" | "group-by" => {
                    MagError::Type(format!("{name} callback must return String: {error}"))
                }
                "filter" => MagError::Type(format!("filter callback must return Bool: {error}")),
                _ => error,
            }
        })?;
    let MagType::Function(_, result) = &callback.ty else {
        unreachable!("callback expectation guarantees a function")
    };
    let output = match name {
        "filter" | "sort-by" => collection.ty.clone(),
        "group-by" => MagType::Map(Box::new(MagType::String), Box::new(collection.ty.clone())),
        "flat-map" => (**result).clone(),
        "map" | "indexed-map" => MagType::List(result.clone()),
        _ => unreachable!(),
    };
    let callee_type = MagType::Function(
        vec![callback.ty.clone(), collection.ty.clone()],
        Box::new(output.clone()),
    );
    Ok(checked(
        output,
        CheckedExprKind::Call {
            callee: Box::new(checked(callee_type, CheckedExprKind::BindingRef(id))),
            args: vec![callback, collection],
        },
    ))
}

fn compile_fold_builtin(
    env: &Env,
    scopes: &[CheckedScope],
    id: BindingId,
    expressions: &[Expr],
) -> Result<CheckedExpr, MagError> {
    if expressions.len() != 3 {
        return Err(MagError::Arity {
            expected: 3,
            got: expressions.len(),
        });
    }
    let init = compile_expr(env, scopes, &expressions[1], None)?;
    let collection = compile_expr(env, scopes, &expressions[2], None)?;
    let MagType::List(item) = &collection.ty else {
        return Err(MagError::Type("fold expects List".into()));
    };
    let callback_expected = MagType::Function(
        vec![init.ty.clone(), (**item).clone()],
        Box::new(init.ty.clone()),
    );
    let callback = compile_expr(env, scopes, &expressions[0], Some(&callback_expected))?;
    let output = init.ty.clone();
    let callee_type = MagType::Function(
        vec![callback.ty.clone(), init.ty.clone(), collection.ty.clone()],
        Box::new(output.clone()),
    );
    Ok(checked(
        output,
        CheckedExprKind::Call {
            callee: Box::new(checked(callee_type, CheckedExprKind::BindingRef(id))),
            args: vec![callback, init, collection],
        },
    ))
}

#[derive(Clone)]
struct ParsedFunction {
    type_params: Vec<String>,
    params: Vec<(String, MagType)>,
    result: MagType,
    body: Vec<Expr>,
}

fn parse_function(
    env: &Env,
    scopes: &[CheckedScope],
    expression: &Expr,
) -> Result<ParsedFunction, MagError> {
    let Expr::List(items) = expression else {
        return Err(MagError::Type("expected fn".into()));
    };
    let args = &items[1..];
    if args.len() < 4 {
        return Err(MagError::Type("typed fn signature required".into()));
    }
    let empty = Expr::Vector(Vec::new());
    let (type_params, params, arrow, result, body) = if matches!(args.get(1), Some(Expr::Vector(_)))
    {
        (&args[0], &args[1], &args[2], &args[3], &args[4..])
    } else {
        (&empty, &args[0], &args[1], &args[2], &args[3..])
    };
    if !matches!(arrow, Expr::Symbol(symbol) if symbol == "->") {
        return Err(MagError::Type("fn signature requires ->".into()));
    }
    let type_params = match type_params {
        Expr::Vector(parameters) => parameters
            .iter()
            .map(|parameter| {
                parameter
                    .as_symbol()
                    .map(str::to_owned)
                    .ok_or_else(|| MagError::Type("generic binder must be a symbol".into()))
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => unreachable!(),
    };
    let mut vars = visible_type_variables(scopes);
    vars.extend(type_params.iter().cloned());
    let params = match params {
        Expr::Vector(params) => params
            .iter()
            .map(|parameter| match parameter {
                Expr::Vector(pair) if pair.len() == 2 => Ok((
                    pair[0]
                        .as_symbol()
                        .ok_or_else(|| MagError::Type("parameter name must be a symbol".into()))?
                        .to_owned(),
                    crate::eval::parse_type(env, &pair[1], &vars)?,
                )),
                _ => Err(MagError::Type("parameter must be [name Type]".into())),
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err(MagError::Type("fn parameters must be a vector".into())),
    };
    Ok(ParsedFunction {
        type_params,
        params,
        result: crate::eval::parse_type(env, result, &vars)?,
        body: body.to_vec(),
    })
}

fn compile_function(
    env: &Env,
    scopes: &[CheckedScope],
    name: Option<&str>,
    expression: &Expr,
    expected: Option<&MagType>,
) -> Result<CheckedExpr, MagError> {
    let function = parse_function(env, scopes, expression)?;
    let signature = MagType::Function(
        function.params.iter().map(|(_, ty)| ty.clone()).collect(),
        Box::new(function.result.clone()),
    );
    if let Some(expected) = expected {
        compatible_static(env, &signature, expected, &mut HashMap::new())
            .map_err(MagError::Type)?;
    }
    let mut parameter_scope = CheckedScope::new();
    if !function.type_params.is_empty() {
        parameter_scope.insert(
            TYPE_BINDER_SCOPE_KEY.into(),
            vec![CheckedCandidate {
                id: BindingId(u64::MAX),
                ty: MagType::Product(
                    function
                        .type_params
                        .iter()
                        .cloned()
                        .map(MagType::Var)
                        .collect(),
                ),
                generic_binders: vec![],
                contributes_type_vars: true,
            }],
        );
    }
    let mut checked_params = Vec::with_capacity(function.params.len());
    for (parameter_name, ty) in function.params {
        let id = env.allocate_binding_id(&parameter_name, Some(ty.clone()));
        insert_checked_candidate(
            env,
            scopes,
            &mut parameter_scope,
            &parameter_name,
            CheckedCandidate {
                id,
                ty: ty.clone(),
                generic_binders: vec![],
                contributes_type_vars: false,
            },
        )?;
        checked_params.push(CheckedParam {
            id,
            name: parameter_name,
            ty,
        });
    }
    let mut body_scopes = scopes.to_vec();
    body_scopes.push(parameter_scope);
    let body = compile_block_in(env, &body_scopes, &function.body, Some(&function.result))?;
    let actual = body
        .expressions
        .last()
        .map(|expression| expression.ty.clone())
        .unwrap_or(MagType::Unit);
    compatible_static(env, &actual, &function.result, &mut HashMap::new()).map_err(|message| {
        MagError::Type(format!(
            "function {}returns {actual}, declared {}: {message}",
            name.map(|name| format!("{name} ")).unwrap_or_default(),
            function.result
        ))
    })?;
    Ok(checked(
        signature,
        CheckedExprKind::Function(Arc::new(CheckedFn {
            name: name.map(str::to_owned),
            type_params: function.type_params,
            params: checked_params,
            result: function.result,
            body: Arc::new(body),
        })),
    ))
}

fn parse_checked_type(
    env: &Env,
    scopes: &[CheckedScope],
    expression: &Expr,
) -> Result<MagType, MagError> {
    crate::eval::parse_type(env, expression, &visible_type_variables(scopes))
}

fn visible_type_variables(scopes: &[CheckedScope]) -> HashSet<String> {
    let mut variables = HashSet::new();
    for candidate in scopes.iter().flat_map(|scope| scope.values()).flatten() {
        if candidate.contributes_type_vars {
            collect_vars(&candidate.ty, &mut variables);
        }
    }
    variables
}

fn has_type_variables(ty: &MagType) -> bool {
    let mut variables = HashSet::new();
    collect_vars(ty, &mut variables);
    !variables.is_empty()
}

fn contains_union(ty: &MagType) -> bool {
    match ty {
        MagType::Union(_) => true,
        MagType::Named(_, arguments) | MagType::Product(arguments) => {
            arguments.iter().any(contains_union)
        }
        MagType::TypeTag(value) | MagType::List(value) => contains_union(value),
        MagType::Map(key, value) => contains_union(key) || contains_union(value),
        MagType::Record(fields) => fields.values().any(contains_union),
        MagType::Function(parameters, result) => {
            parameters.iter().any(contains_union) || contains_union(result)
        }
        _ => false,
    }
}

fn instantiate_candidate(candidate: &CheckedCandidate) -> (MagType, HashSet<String>) {
    let substitutions = candidate
        .generic_binders
        .iter()
        .enumerate()
        .map(|(index, binder)| {
            (
                binder.clone(),
                MagType::Var(format!("\0binding{}.{index}", candidate.id.0)),
            )
        })
        .collect::<HashMap<_, _>>();
    let bindable = substitutions
        .values()
        .filter_map(|ty| match ty {
            MagType::Var(name) => Some(name.clone()),
            _ => None,
        })
        .collect();
    (substitute(&candidate.ty, &substitutions), bindable)
}

fn internal_type_variables(ty: &MagType) -> HashSet<String> {
    let mut variables = HashSet::new();
    collect_vars(ty, &mut variables);
    variables.retain(|name| name.starts_with('\0'));
    variables
}

fn canonical_type(ty: &MagType) -> String {
    fn canonicalize(
        ty: &MagType,
        variables: &mut HashMap<String, MagType>,
        next: &mut usize,
    ) -> MagType {
        match ty {
            MagType::Var(name) => variables
                .entry(name.clone())
                .or_insert_with(|| {
                    let variable = MagType::Var(format!("${next}"));
                    *next += 1;
                    variable
                })
                .clone(),
            MagType::Named(name, args) => MagType::Named(
                name.clone(),
                args.iter()
                    .map(|arg| canonicalize(arg, variables, next))
                    .collect(),
            ),
            MagType::TypeTag(item) => {
                MagType::TypeTag(Box::new(canonicalize(item, variables, next)))
            }
            MagType::List(item) => MagType::List(Box::new(canonicalize(item, variables, next))),
            MagType::Map(key, value) => MagType::Map(
                Box::new(canonicalize(key, variables, next)),
                Box::new(canonicalize(value, variables, next)),
            ),
            MagType::Record(fields) => MagType::Record(
                fields
                    .iter()
                    .map(|(name, ty)| (name.clone(), canonicalize(ty, variables, next)))
                    .collect(),
            ),
            MagType::Union(items) => MagType::Union(
                items
                    .iter()
                    .map(|item| canonicalize(item, variables, next))
                    .collect(),
            ),
            MagType::Product(items) => MagType::Product(
                items
                    .iter()
                    .map(|item| canonicalize(item, variables, next))
                    .collect(),
            ),
            MagType::Function(params, result) => MagType::Function(
                params
                    .iter()
                    .map(|param| canonicalize(param, variables, next))
                    .collect(),
                Box::new(canonicalize(result, variables, next)),
            ),
            _ => ty.clone(),
        }
    }
    canonicalize(ty, &mut HashMap::new(), &mut 0).to_string()
}

pub(crate) fn value_type(value: &Value) -> Option<MagType> {
    match value {
        Value::Unit => Some(MagType::Unit),
        Value::Bool(_) => Some(MagType::Bool),
        Value::Int(_) => Some(MagType::Int),
        Value::Float(_) => Some(MagType::Float),
        Value::Str(_) | Value::Keyword(_) | Value::Symbol(_) => Some(MagType::String),
        Value::List(v) | Value::Vector(v) if v.is_empty() => Some(MagType::EmptyList),
        Value::List(v) | Value::Vector(v) => {
            Some(MagType::List(Box::new(v.first().and_then(value_type)?)))
        }
        Value::Product(v) => Some(MagType::Product(
            v.iter().map(value_type).collect::<Option<Vec<_>>>()?,
        )),
        Value::Map(m) => Some(MagType::Record(
            m.iter()
                .filter_map(|(k, v)| Some((k.clone(), value_type(v)?)))
                .collect(),
        )),
        Value::Fn(f) => Some(MagType::Function(
            f.param_types.clone(),
            Box::new(f.return_type.clone()),
        )),
        Value::Type(t) => Some(t.clone()),
        Value::TypeDecl(d) => Some(MagType::Named(d.name.clone(), vec![])),
        Value::TypeTag(ty) => Some(MagType::TypeTag(Box::new(ty.to_mag_type()))),
        Value::TypeDescriptor(_) => Some(MagType::TypeDescriptor),
        Value::TypeSchema(_) => Some(MagType::TypeSchema),
        Value::SemanticTypeId(_) => Some(MagType::SemanticTypeId),
        Value::PackedValue(_) => Some(MagType::PackedValue),
        Value::JsonValue(_) => Some(MagType::JsonValue),
        Value::HostInputs(_) => Some(MagType::HostInputs),
        Value::Artifact(_) => Some(MagType::Artifact),
        Value::Typed(_, ty) => Some(ty.clone()),
        Value::BuiltinFn(_) => None,
    }
}

pub(crate) fn canonical_value_type(value: &Value) -> Option<String> {
    if let Value::Type(ty) = value {
        return Some(format!("Type<{ty}>"));
    }
    if let Value::Fn(function) = value {
        let substitutions = function
            .type_params
            .iter()
            .enumerate()
            .map(|(index, name)| (name.clone(), MagType::Var(format!("${index}"))))
            .collect::<HashMap<_, _>>();
        let params = function
            .param_types
            .iter()
            .map(|ty| substitute(ty, &substitutions).to_string())
            .collect::<Vec<_>>()
            .join(",");
        let result = substitute(&function.return_type, &substitutions);
        return Some(format!(
            "forall[{}].Fn({params})->{result}",
            function.type_params.len()
        ));
    }
    value_type(value).map(|ty| ty.to_string())
}

fn field_type(env: &Env, ty: &MagType, key: Option<&str>) -> Option<MagType> {
    match ty {
        MagType::JsonValue => Some(MagType::JsonValue),
        MagType::Map(_, v) => Some((**v).clone()),
        MagType::Record(fields) => key.and_then(|k| fields.get(k).cloned()),
        MagType::Named(name, args) => env.type_decl(name).and_then(|decl| {
            let substitutions = decl
                .params
                .iter()
                .cloned()
                .zip(args.iter().cloned())
                .collect();
            let body = substitute(&decl.body, &substitutions);
            field_type(env, &body, key)
        }),
        MagType::Union(ts) => {
            let fields = ts
                .iter()
                .filter_map(|t| field_type(env, t, key))
                .collect::<Vec<_>>();
            if fields.is_empty() {
                None
            } else {
                Some(MagType::Union(fields))
            }
        }
        _ => None,
    }
}

fn compatible(
    env: &Env,
    actual: &MagType,
    expected: &MagType,
    subst: &mut HashMap<String, MagType>,
) -> Result<(), String> {
    compatible_in(env, actual, expected, subst, None)
}

fn compatible_static(
    env: &Env,
    actual: &MagType,
    expected: &MagType,
    subst: &mut HashMap<String, MagType>,
) -> Result<(), String> {
    let mut bindable = internal_type_variables(actual);
    bindable.extend(internal_type_variables(expected));
    compatible_in(env, actual, expected, subst, Some(&bindable))
}

fn compatible_with_bindable(
    env: &Env,
    actual: &MagType,
    expected: &MagType,
    subst: &mut HashMap<String, MagType>,
    bindable: &HashSet<String>,
) -> Result<(), String> {
    let mut active = bindable.clone();
    active.extend(internal_type_variables(actual));
    active.extend(internal_type_variables(expected));
    compatible_in(env, actual, expected, subst, Some(&active))
}

fn compatible_in(
    env: &Env,
    actual: &MagType,
    expected: &MagType,
    subst: &mut HashMap<String, MagType>,
    bindable: Option<&HashSet<String>>,
) -> Result<(), String> {
    if let MagType::Var(name) = expected {
        if bindable.is_none_or(|bindable| bindable.contains(name)) {
            if let Some(bound) = subst.get(name) {
                let bound = bound.clone();
                return compatible_in(env, actual, &bound, subst, bindable)
                    .map_err(|_| format!("{name} was {bound}, got {actual}"));
            }
            subst.insert(name.clone(), actual.clone());
            return Ok(());
        }
        if actual == expected {
            return Ok(());
        }
    }
    if let MagType::Var(name) = actual {
        if bindable.is_some_and(|bindable| bindable.contains(name)) {
            if let Some(bound) = subst.get(name) {
                let bound = bound.clone();
                return compatible_in(env, &bound, expected, subst, bindable)
                    .map_err(|_| format!("{name} was {bound}, expected {expected}"));
            }
            subst.insert(name.clone(), expected.clone());
            return Ok(());
        }
    }
    if matches!(expected, MagType::Var(_)) {
        return Err(format!("expected {expected}, got {actual}"));
    }
    if actual == expected {
        return Ok(());
    }
    if matches!(actual, MagType::Never) {
        return Ok(());
    }
    if matches!(actual, MagType::EmptyList) && matches!(expected, MagType::List(_)) {
        return Ok(());
    }
    // Once inference has removed open variables, compatibility is owned by
    // the same normalized descriptor relation emitted to the runtime.
    if let (Ok(actual), Ok(expected)) = (
        crate::types::ConcreteType::resolve(env, actual),
        crate::types::ConcreteType::resolve(env, expected),
    ) {
        return expected.accepts(&actual).then_some(()).ok_or_else(|| {
            if matches!(expected, crate::types::ConcreteType::Named { .. }) {
                format!(
                    "expected nominal {expected:?}, got {actual:?}; use as for explicit refinement"
                )
            } else {
                format!("expected {expected:?}, got {actual:?}")
            }
        });
    }
    if let MagType::Union(variants) = actual {
        for variant in variants {
            compatible_in(env, variant, expected, subst, bindable)?;
        }
        return Ok(());
    }
    match expected {
        MagType::Union(options) => {
            let original_bindings = subst.len();
            let matches = options.iter().filter_map(|option| {
                let mut candidate = subst.clone();
                compatible_in(env, actual, option, &mut candidate, bindable)
                    .ok()
                    .map(|_| (candidate.len() - original_bindings, candidate))
            });
            let matches = matches.collect::<Vec<_>>();
            let specificity = matches
                .iter()
                .map(|(new_bindings, _)| *new_bindings)
                .min()
                .ok_or_else(|| format!("expected {expected}, got {actual}"))?;
            let mut best = matches
                .into_iter()
                .filter(|(new_bindings, _)| *new_bindings == specificity)
                .map(|(_, candidate)| candidate);
            let selected = best
                .next()
                .ok_or_else(|| format!("expected {expected}, got {actual}"))?;
            for candidate in best {
                if candidate != selected {
                    return Err(format!(
                        "ambiguous union match for {actual} against {expected}"
                    ));
                }
            }
            *subst = selected;
            Ok(())
        }
        MagType::Named(expected_name, expected_args) => match actual {
            MagType::Named(actual_name, actual_args)
                if actual_name == expected_name && actual_args.len() == expected_args.len() =>
            {
                for (actual_arg, expected_arg) in actual_args.iter().zip(expected_args) {
                    compatible_in(env, actual_arg, expected_arg, subst, bindable)?;
                }
                Ok(())
            }
            _ => Err(format!(
                "expected nominal {expected}, got {actual}; use as for explicit refinement"
            )),
        },
        MagType::TypeTag(expected_type) => match actual {
            MagType::TypeTag(actual_type) => {
                compatible_in(env, actual_type, expected_type, subst, bindable)
            }
            _ => Err(format!("expected {expected}, got {actual}")),
        },
        MagType::List(e) => match actual {
            MagType::List(a) => compatible_in(env, a, e, subst, bindable),
            _ => Err(format!("expected {expected}, got {actual}")),
        },
        MagType::Map(ek, ev) => match actual {
            MagType::Map(ak, av) => {
                compatible_in(env, ak, ek, subst, bindable)?;
                compatible_in(env, av, ev, subst, bindable)
            }
            MagType::Record(fields) if matches!(ek.as_ref(), MagType::String) => {
                for value in fields.values() {
                    compatible_in(env, value, ev, subst, bindable)?;
                }
                Ok(())
            }
            _ => Err(format!("expected {expected}, got {actual}")),
        },
        MagType::Record(ef) => match actual {
            MagType::Record(af) if ef.len() == af.len() => {
                for (k, e) in ef {
                    compatible_in(
                        env,
                        af.get(k).ok_or_else(|| format!("missing field {k}"))?,
                        e,
                        subst,
                        bindable,
                    )?;
                }
                Ok(())
            }
            _ => Err(format!("expected {expected}, got {actual}")),
        },
        MagType::Product(expected_items) => match actual {
            MagType::Product(actual_items) if actual_items.len() == expected_items.len() => {
                for (actual_item, expected_item) in actual_items.iter().zip(expected_items) {
                    compatible_in(env, actual_item, expected_item, subst, bindable)?;
                }
                Ok(())
            }
            _ => Err(format!("expected {expected}, got {actual}")),
        },
        MagType::Function(ep, er) => match actual {
            MagType::Function(ap, ar) if ap.len() == ep.len() => {
                for (a, e) in ap.iter().zip(ep) {
                    compatible_in(env, a, e, subst, bindable)?;
                }
                compatible_in(env, ar, er, subst, bindable)
            }
            _ => Err(format!("expected {expected}, got {actual}")),
        },
        _ => Err(format!("expected {expected}, got {actual}")),
    }
}
pub(crate) fn substitute(ty: &MagType, subst: &HashMap<String, MagType>) -> MagType {
    match ty {
        MagType::Var(n) => subst.get(n).cloned().unwrap_or_else(|| ty.clone()),
        MagType::Named(n, a) => {
            MagType::Named(n.clone(), a.iter().map(|t| substitute(t, subst)).collect())
        }
        MagType::TypeTag(t) => MagType::TypeTag(Box::new(substitute(t, subst))),
        MagType::List(t) => MagType::List(Box::new(substitute(t, subst))),
        MagType::Map(k, v) => MagType::Map(
            Box::new(substitute(k, subst)),
            Box::new(substitute(v, subst)),
        ),
        MagType::Record(f) => MagType::Record(
            f.iter()
                .map(|(k, v)| (k.clone(), substitute(v, subst)))
                .collect(),
        ),
        MagType::Union(v) => MagType::Union(v.iter().map(|t| substitute(t, subst)).collect()),
        MagType::Product(v) => MagType::Product(v.iter().map(|t| substitute(t, subst)).collect()),
        MagType::Function(p, r) => MagType::Function(
            p.iter().map(|t| substitute(t, subst)).collect(),
            Box::new(substitute(r, subst)),
        ),
        _ => ty.clone(),
    }
}

fn collect_vars(ty: &MagType, out: &mut HashSet<String>) {
    match ty {
        MagType::Var(name) => {
            out.insert(name.clone());
        }
        MagType::Named(_, args) | MagType::Union(args) | MagType::Product(args) => {
            for arg in args {
                collect_vars(arg, out);
            }
        }
        MagType::List(item) => collect_vars(item, out),
        MagType::TypeTag(item) => collect_vars(item, out),
        MagType::Map(key, value) => {
            collect_vars(key, out);
            collect_vars(value, out);
        }
        MagType::Record(fields) => {
            for field in fields.values() {
                collect_vars(field, out);
            }
        }
        MagType::Function(params, result) => {
            for param in params {
                collect_vars(param, out);
            }
            collect_vars(result, out);
        }
        _ => {}
    }
}

#[cfg(test)]
mod builtin_signature_tests {
    use super::*;

    #[test]
    fn every_visible_builtin_has_collision_signatures() {
        let env = Env::new_with_stdlib();
        for &name in BUILTIN_NAMES {
            let signatures = if name == "str" {
                let representative =
                    MagType::Function(vec![MagType::Int], Box::new(MagType::String));
                builtin_overload_types(name, Some(&representative))
            } else {
                builtin_overload_types(name, None)
            };
            assert!(
                !signatures.is_empty(),
                "visible builtin {name} has no signature inventory"
            );
            for signature in signatures {
                assert!(
                    collides_with_builtin(&env, name, &signature),
                    "visible builtin {name} does not recognize signature {signature}"
                );
            }
        }
    }

    #[test]
    fn record_builtin_families_are_in_the_collision_inventory() {
        let env = Env::new_with_stdlib();
        let record = MagType::Record(BTreeMap::from([("value".into(), MagType::Int)]));
        for (name, candidate) in [
            (
                "count",
                MagType::Function(vec![record.clone()], Box::new(MagType::Int)),
            ),
            (
                "keys",
                MagType::Function(
                    vec![record.clone()],
                    Box::new(MagType::List(Box::new(MagType::String))),
                ),
            ),
            (
                "get",
                MagType::Function(
                    vec![record.clone(), MagType::String],
                    Box::new(MagType::Int),
                ),
            ),
            (
                "assoc",
                MagType::Function(
                    vec![record.clone(), MagType::String, MagType::Int],
                    Box::new(record.clone()),
                ),
            ),
        ] {
            assert!(collides_with_builtin(&env, name, &candidate), "{name}");
        }
    }

    #[test]
    fn generic_product_components_are_inferred_positionally() {
        let env = Env::new_with_stdlib();
        let actual = MagType::Product(vec![MagType::String, MagType::String]);
        let expected = MagType::Product(vec![MagType::Var("A".into()), MagType::Var("B".into())]);
        let bindable = HashSet::from(["A".into(), "B".into()]);
        let mut substitution = HashMap::new();
        compatible_in(&env, &actual, &expected, &mut substitution, Some(&bindable))
            .expect("a product should infer each generic occurrence");
        assert_eq!(substitution.get("A"), Some(&MagType::String));
        assert_eq!(substitution.get("B"), Some(&MagType::String));
    }
}
