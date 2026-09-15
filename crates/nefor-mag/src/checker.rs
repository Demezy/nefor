use crate::ast::{
    BindingId, CheckedBinding, CheckedBlock, CheckedExpr, CheckedExprKind, CheckedFn,
    CheckedMatchArm, CheckedParam, ConstructorDecl, ConstructorDeclarationId, TypeDeclBody, Value,
};
use crate::authored::{BlockItem, Expr, Function, Type};
use crate::env::Env;
use crate::error::MagError;
use crate::types::MagType;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

thread_local! {
    static EQUALITY_OBLIGATIONS: RefCell<Vec<HashSet<String>>> = const { RefCell::new(Vec::new()) };
}

fn with_equality_obligation_scope<T>(
    operation: impl FnOnce() -> Result<T, MagError>,
) -> Result<(T, HashSet<String>), MagError> {
    EQUALITY_OBLIGATIONS.with(|scopes| scopes.borrow_mut().push(HashSet::new()));
    let result = operation();
    let obligations =
        EQUALITY_OBLIGATIONS.with(|scopes| scopes.borrow_mut().pop().unwrap_or_default());
    result.map(|value| (value, obligations))
}

fn record_equality_variables(variables: impl IntoIterator<Item = String>) {
    EQUALITY_OBLIGATIONS.with(|scopes| {
        if let Some(scope) = scopes.borrow_mut().last_mut() {
            scope.extend(variables);
        }
    });
}

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

#[cfg(test)]
pub(crate) fn compile_resolved_function(
    env: &Env,
    name: Option<&str>,
    type_params: &[String],
    params: &[String],
    param_types: &[MagType],
    result: &MagType,
    body: &[BlockItem],
) -> Result<CheckedFn, MagError> {
    let mut parameter_scope = CheckedScope::new();
    if !type_params.is_empty() {
        parameter_scope.insert(
            TYPE_BINDER_SCOPE_KEY.into(),
            vec![CheckedCandidate {
                id: BindingId(u64::MAX),
                ty: MagType::Product(type_params.iter().cloned().map(MagType::Var).collect()),
                generic_binders: vec![],
                contributes_type_vars: true,
            }],
        );
    }
    let mut checked_params = Vec::with_capacity(params.len());
    for (parameter_name, ty) in params.iter().zip(param_types) {
        let id = env.allocate_binding_id(parameter_name, Some(ty.clone()));
        insert_checked_candidate(
            env,
            &[],
            &mut parameter_scope,
            parameter_name,
            CheckedCandidate {
                id,
                ty: ty.clone(),
                generic_binders: vec![],
                contributes_type_vars: false,
            },
        )?;
        checked_params.push(CheckedParam {
            id,
            name: parameter_name.clone(),
            ty: ty.clone(),
        });
    }
    let (checked_body, equality_variables) = with_equality_obligation_scope(|| {
        compile_block_in(env, &[parameter_scope], body, Some(result))
    })?;
    let equality_params = type_params
        .iter()
        .filter(|parameter| equality_variables.contains(*parameter))
        .cloned()
        .collect::<Vec<_>>();
    let actual = checked_body
        .expressions
        .last()
        .map(|expression| expression.ty.clone())
        .unwrap_or(MagType::Unit);
    compatible_static(env, &actual, result, &mut HashMap::new()).map_err(|message| {
        MagError::Type(format!(
            "function {}returns {actual}, declared {result}: {message}",
            name.map(|name| format!("{name} ")).unwrap_or_default()
        ))
    })?;
    Ok(CheckedFn {
        name: name.map(str::to_owned),
        type_params: type_params.to_vec(),
        equality_params,
        params: checked_params,
        result: result.clone(),
        body: Arc::new(checked_body),
    })
}

fn direct_let(item: &BlockItem) -> Result<Option<(&str, &Expr)>, MagError> {
    match item {
        BlockItem::Let { name, value } => Ok(Some((name, value))),
        BlockItem::Expr(_) => Ok(None),
        BlockItem::Invalid(error) => Err(error.clone().into_mag_error()),
    }
}

fn block_expr(item: &BlockItem) -> Result<Option<&Expr>, MagError> {
    match item {
        BlockItem::Expr(expression) => Ok(Some(expression)),
        BlockItem::Let { .. } => Ok(None),
        BlockItem::Invalid(error) => Err(error.clone().into_mag_error()),
    }
}

fn is_fn(expr: &Expr) -> bool {
    matches!(expr, Expr::Function(_))
}

fn function_type_params(expression: &Expr) -> Result<Vec<String>, MagError> {
    match expression {
        Expr::Function(function) => Ok(function.type_params.clone()),
        Expr::Invalid(error) => Err(error.clone().into_mag_error()),
        _ => Ok(vec![]),
    }
}

fn infer_fn_signature(env: &Env, outer: &Locals, expression: &Expr) -> Result<MagType, MagError> {
    let scoped_type_vars = outer
        .values()
        .flatten()
        .flat_map(|ty| {
            let mut vars = HashSet::new();
            collect_vars(ty, &mut vars);
            vars
        })
        .collect();
    infer_fn_signature_scoped(env, &scoped_type_vars, expression)
}

fn infer_fn_signature_scoped(
    env: &Env,
    scoped_type_vars: &HashSet<String>,
    expression: &Expr,
) -> Result<MagType, MagError> {
    let Expr::Function(function) = expression else {
        return match expression {
            Expr::Invalid(error) => Err(error.clone().into_mag_error()),
            _ => Err(MagError::Type("typed fn signature required".into())),
        };
    };
    let mut vars = scoped_type_vars.clone();
    vars.extend(function.type_params.iter().cloned());
    let params = function
        .params
        .iter()
        .map(|parameter| resolve_type(env, &parameter.ty, &vars))
        .collect::<Result<Vec<_>, _>>()?;
    let result = resolve_type(env, &function.result, &vars)?;
    Ok(MagType::Function(params, Box::new(result)))
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
    validate_equality_requirements(
        env,
        &function
            .equality_params
            .iter()
            .cloned()
            .map(MagType::Var)
            .collect::<Vec<_>>(),
        &subst,
    )?;
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
    validate_equality_requirements(
        env,
        &function
            .equality_params
            .iter()
            .cloned()
            .map(MagType::Var)
            .collect::<Vec<_>>(),
        &substitution,
    )?;
    Ok((result.as_ref().clone(), substitution))
}

fn infer(env: &Env, locals: &mut Locals, expr: &Expr) -> Result<MagType, MagError> {
    match expr {
        Expr::Unit => Ok(MagType::Unit),
        Expr::Bool(_) => Ok(MagType::Bool),
        Expr::Int(_) => Ok(MagType::Int),
        Expr::Float(_) => Ok(MagType::Float),
        Expr::Str(_) | Expr::Keyword(_) => Ok(MagType::String),
        Expr::Name(name) => match locals.get(name).map(Vec::as_slice) {
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
        Expr::Vector(items) => infer_list(env, locals, items),
        Expr::Record(fields) => {
            let mut inferred = BTreeMap::new();
            for (key, value) in fields {
                if inferred
                    .insert(key.clone(), infer(env, locals, value)?)
                    .is_some()
                {
                    return Err(MagError::Type(format!("duplicate record field {key}")));
                }
            }
            Ok(MagType::Record(inferred))
        }
        Expr::If {
            condition,
            then_branch,
            else_branch,
        } => {
            compatible(
                env,
                &infer(env, locals, condition)?,
                &MagType::Bool,
                &mut HashMap::new(),
            )
            .map_err(MagError::Type)?;
            let left = infer(env, locals, then_branch)?;
            let right = infer(env, locals, else_branch)?;
            compatible(env, &left, &right, &mut HashMap::new()).map_err(|_| {
                MagError::Type(format!(
                    "if branches must return one compatible type, got {left} and {right}"
                ))
            })?;
            Ok(right)
        }
        Expr::Construct {
            owner,
            constructor,
            payload,
        } => infer_construct(env, locals, owner, constructor, payload),
        Expr::Match { value, arms } => infer_match(env, locals, value, arms),
        Expr::Ascribe { target, value } => {
            let mut vars = HashSet::new();
            for ty in locals.values().flatten() {
                collect_vars(ty, &mut vars);
            }
            let target = resolve_type(env, target, &vars)?;
            match value.as_ref() {
                Expr::Name(name) if locals.get(name).is_some_and(|types| types.len() > 1) => {
                    let matches = locals[name]
                        .iter()
                        .filter(|ty| compatible(env, ty, &target, &mut HashMap::new()).is_ok())
                        .collect::<Vec<_>>();
                    match matches.as_slice() {
                        [_] => {}
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
                Expr::Name(name) if env.lookup_candidates(name).len() > 1 => {
                    let _ = env.lookup_by_type(name, &target)?;
                }
                _ => {}
            }
            Ok(target)
        }
        Expr::TypeTag(target) => {
            let mut vars = HashSet::new();
            for ty in locals.values().flatten() {
                collect_vars(ty, &mut vars);
            }
            Ok(MagType::TypeTag(Box::new(resolve_type(
                env, target, &vars,
            )?)))
        }
        Expr::Function(_) => infer_fn_signature(env, locals, expr),
        Expr::Call { callee, args } => infer_call(env, locals, callee, args),
        Expr::Invalid(error) => Err(error.clone().into_mag_error()),
    }
}

fn infer_list(env: &Env, locals: &mut Locals, items: &[Expr]) -> Result<MagType, MagError> {
    if items.is_empty() {
        return Ok(MagType::EmptyList);
    }
    let first = infer(env, locals, &items[0])?;
    for item in &items[1..] {
        let ty = infer(env, locals, item)?;
        compatible(env, &ty, &first, &mut HashMap::new()).map_err(MagError::Type)?;
    }
    Ok(MagType::List(Box::new(first)))
}

fn infer_call(
    env: &Env,
    locals: &mut Locals,
    callee: &Expr,
    args: &[Expr],
) -> Result<MagType, MagError> {
    if let Expr::Name(name) = callee {
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
            let argument_types = args
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
                        compatible(
                            env,
                            &argument_types[index],
                            &substitute(&params[index], &substitution),
                            &mut substitution,
                        )
                        .ok()?;
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
                [] if builtin => infer_builtin(env, locals, name, args),
                [] => Err(MagError::Type(format!("no overload {name} matches call"))),
                _ => Err(MagError::Type(format!(
                    "ambiguous overload {name} for call"
                ))),
            };
        } else if builtin {
            return infer_builtin(env, locals, name, args);
        }
    }
    let callable = infer(env, locals, callee)?;
    let MagType::Function(params, result) = callable else {
        return Err(MagError::Type(format!("cannot call {callable}")));
    };
    if params.len() != args.len() {
        return Err(MagError::Type(format!(
            "call expects {} arguments, got {}",
            params.len(),
            args.len()
        )));
    }
    let argument_types = args
        .iter()
        .map(|argument| infer(env, locals, argument))
        .collect::<Result<Vec<_>, _>>()?;
    let mut substitution = HashMap::new();
    let mut order = (0..params.len()).collect::<Vec<_>>();
    order.sort_by_key(|index| contains_union(&params[*index]));
    for index in order {
        compatible(
            env,
            &argument_types[index],
            &substitute(&params[index], &substitution),
            &mut substitution,
        )
        .map_err(MagError::Type)?;
    }
    Ok(substitute(&result, &substitution))
}

fn infer_construct(
    env: &Env,
    locals: &mut Locals,
    owner: &Type,
    constructor: &str,
    payload: &Expr,
) -> Result<MagType, MagError> {
    let mut vars = HashSet::new();
    for ty in locals.values().flatten() {
        collect_vars(ty, &mut vars);
    }
    let owner = resolve_type(env, owner, &vars)?;
    let (_, payload_type) = instantiated_constructor(env, &owner, constructor)?;
    let actual = infer(env, locals, payload)?;
    compatible(env, &actual, &payload_type, &mut HashMap::new()).map_err(MagError::Type)?;
    Ok(owner)
}

fn infer_match(
    env: &Env,
    locals: &Locals,
    value: &Expr,
    arms: &[crate::authored::MatchArm],
) -> Result<MagType, MagError> {
    let owner = infer(env, &mut locals.clone(), value)?;
    let constructors = adt_constructors(env, &owner)?;
    let mut seen = HashSet::new();
    let mut result = None;
    for arm in arms {
        let (constructor, payload_type) = instantiated_constructor(env, &owner, &arm.constructor)?;
        if !seen.insert(constructor.clone()) {
            return Err(MagError::Type(format!(
                "duplicate match arm for {}",
                arm.constructor
            )));
        }
        let mut arm_locals = locals.clone();
        add_local(&mut arm_locals, arm.binding.clone(), payload_type)?;
        let body_type = infer(env, &mut arm_locals, &arm.body)?;
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
    ensure_adt_exhaustive(&constructors, &seen)?;
    Ok(result.unwrap_or(MagType::Unit))
}

fn require_equality_admissible(env: &Env, ty: &MagType) -> Result<(), MagError> {
    let mut variables = HashSet::new();
    equality_admissible_in(env, ty, &mut variables, &mut HashSet::new()).map_err(MagError::Type)?;
    record_equality_variables(variables);
    Ok(())
}

fn equality_admissible_in(
    env: &Env,
    ty: &MagType,
    variables: &mut HashSet<String>,
    visiting: &mut HashSet<MagType>,
) -> Result<(), String> {
    match ty {
        MagType::Var(name) => {
            variables.insert(name.clone());
            Ok(())
        }
        MagType::Unit
        | MagType::Bool
        | MagType::Int
        | MagType::Float
        | MagType::String
        | MagType::JsonValue
        | MagType::Never
        | MagType::EmptyList => Ok(()),
        MagType::List(item) | MagType::Set(item) => {
            equality_admissible_in(env, item, variables, visiting)
        }
        MagType::Map(key, value) => {
            equality_admissible_in(env, key, variables, visiting)?;
            equality_admissible_in(env, value, variables, visiting)
        }
        MagType::Record(fields) => fields
            .values()
            .try_for_each(|field| equality_admissible_in(env, field, variables, visiting)),
        MagType::Product(items) => items
            .iter()
            .try_for_each(|item| equality_admissible_in(env, item, variables, visiting)),
        MagType::Named(name, arguments) => {
            for argument in arguments {
                equality_admissible_in(env, argument, variables, visiting)?;
            }
            if !visiting.insert(ty.clone()) {
                return Ok(());
            }
            let declaration = env
                .type_decl(name)
                .ok_or_else(|| format!("unknown nominal type {name}"))?;
            let substitution = declaration
                .params
                .iter()
                .cloned()
                .zip(arguments.iter().cloned())
                .collect::<HashMap<_, _>>();
            let result = match declaration.body {
                TypeDeclBody::Nominal(body) => equality_admissible_in(
                    env,
                    &substitute(&body, &substitution),
                    variables,
                    visiting,
                ),
                TypeDeclBody::Adt(constructors) => {
                    constructors.iter().try_for_each(|constructor| {
                        equality_admissible_in(
                            env,
                            &substitute(&constructor.payload, &substitution),
                            variables,
                            visiting,
                        )
                    })
                }
                TypeDeclBody::Native => Err(format!(
                    "type {ty} has opaque native behavior and does not support equality"
                )),
            };
            visiting.remove(ty);
            result
        }
        MagType::Function(_, _) => Err(format!("function type {ty} does not support equality")),
        MagType::Artifact
        | MagType::TypeDescriptor
        | MagType::TypeSchema
        | MagType::SemanticTypeId
        | MagType::PackedValue
        | MagType::HostInputs
        | MagType::TypeTag(_) => Err(format!("type {ty} does not support equality")),
    }
}

fn validate_equality_requirements(
    env: &Env,
    requirements: &[MagType],
    substitution: &HashMap<String, MagType>,
) -> Result<(), MagError> {
    for requirement in requirements {
        require_equality_admissible(env, &substitute(requirement, substitution))?;
    }
    Ok(())
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
        "__map-empty" => {
            if !(1..=2).contains(&args.len()) {
                return Err(MagError::Type(format!(
                    "__map-empty expects 1-2 arguments, got {}",
                    args.len()
                )));
            }
            let key = match infer(env, locals, &args[0])? {
                MagType::TypeTag(key) => *key,
                actual => {
                    return Err(MagError::Type(format!(
                        "__map-empty expects TypeTag keys, got {actual}"
                    )))
                }
            };
            require_equality_admissible(env, &key)?;
            let value = if let Some(value) = args.get(1) {
                match infer(env, locals, value)? {
                    MagType::TypeTag(value) => *value,
                    actual => {
                        return Err(MagError::Type(format!(
                            "__map-empty expects TypeTag values, got {actual}"
                        )))
                    }
                }
            } else {
                MagType::Var("\0builtin.value".into())
            };
            Ok(MagType::Map(Box::new(key), Box::new(value)))
        }
        "__map-insert" | "__map-put" | "__map-get-or" | "__map-get" | "__map-contains" => {
            exact(
                if matches!(name, "__map-insert" | "__map-put" | "__map-get-or") {
                    3
                } else {
                    2
                },
            )?;
            let target = infer(env, locals, &args[0])?;
            let MagType::Map(key, value) = target else {
                return Err(MagError::Type(format!("{name} expects Map, got {target}")));
            };
            require_equality_admissible(env, &key)?;
            let actual_key = infer(env, locals, &args[1])?;
            compatible(env, &actual_key, &key, &mut HashMap::new()).map_err(MagError::Type)?;
            if matches!(name, "__map-insert" | "__map-put" | "__map-get-or") {
                let actual_value = infer(env, locals, &args[2])?;
                compatible(env, &actual_value, &value, &mut HashMap::new())
                    .map_err(MagError::Type)?;
                if name == "__map-get-or" {
                    Ok(*value)
                } else {
                    Ok(MagType::Map(key, value))
                }
            } else if name == "__map-get" {
                Ok(*value)
            } else {
                Ok(MagType::Bool)
            }
        }
        "__map-union-left" => {
            exact(2)?;
            let left = infer(env, locals, &args[0])?;
            let right = infer(env, locals, &args[1])?;
            compatible(env, &right, &left, &mut HashMap::new()).map_err(MagError::Type)?;
            let MagType::Map(key, value) = left else {
                return Err(MagError::Type(format!(
                    "__map-union-left expects Map, got {left}"
                )));
            };
            require_equality_admissible(env, &key)?;
            Ok(MagType::Map(key, value))
        }
        "__map-count" => {
            exact(1)?;
            match infer(env, locals, &args[0])? {
                MagType::Map(key, _) => {
                    require_equality_admissible(env, &key)?;
                    Ok(MagType::Int)
                }
                actual => Err(MagError::Type(format!(
                    "__map-count expects Map, got {actual}"
                ))),
            }
        }
        "__set-empty" => {
            exact(1)?;
            let item = match infer(env, locals, &args[0])? {
                MagType::TypeTag(item) => *item,
                actual => {
                    return Err(MagError::Type(format!(
                        "__set-empty expects TypeTag, got {actual}"
                    )))
                }
            };
            require_equality_admissible(env, &item)?;
            Ok(MagType::Set(Box::new(item)))
        }
        "__set-insert" | "__set-contains" => {
            exact(2)?;
            let target = infer(env, locals, &args[0])?;
            let MagType::Set(item) = target else {
                return Err(MagError::Type(format!("{name} expects Set, got {target}")));
            };
            require_equality_admissible(env, &item)?;
            let actual = infer(env, locals, &args[1])?;
            compatible(env, &actual, &item, &mut HashMap::new()).map_err(MagError::Type)?;
            if name == "__set-insert" {
                Ok(MagType::Set(item))
            } else {
                Ok(MagType::Bool)
            }
        }
        "__set-count" => {
            exact(1)?;
            match infer(env, locals, &args[0])? {
                MagType::Set(item) => {
                    require_equality_admissible(env, &item)?;
                    Ok(MagType::Int)
                }
                actual => Err(MagError::Type(format!(
                    "__set-count expects Set, got {actual}"
                ))),
            }
        }
        "get" => {
            exact(2)?;
            let target = infer(env, locals, &args[0])?;
            if matches!(target, MagType::HostInputs | MagType::Map(_, _)) {
                return Err(MagError::Type(format!(
                    "get expects a record, got {target}"
                )));
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
            if matches!(target, MagType::Map(_, _)) {
                return Err(MagError::Type("assoc expects a record".into()));
            }
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
                MagType::List(_) | MagType::Record(_) | MagType::String => Ok(MagType::Int),
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
            require_equality_admissible(env, &left)?;
            require_equality_admissible(env, &right)?;
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
            compatible(env, &a, &b, &mut HashMap::new()).map_err(|_| {
                MagError::Type(format!(
                    "or operands must return one compatible type, got {a} and {b}"
                ))
            })?;
            Ok(b)
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
                MagType::Record(_) => Ok(MagType::List(Box::new(MagType::String))),
                actual => Err(MagError::Type(format!(
                    "keys expects a record, got {actual}"
                ))),
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
    "__map-empty",
    "__map-insert",
    "__map-put",
    "__map-get",
    "__map-get-or",
    "__map-contains",
    "__map-count",
    "__map-union-left",
    "__set-empty",
    "__set-insert",
    "__set-contains",
    "__set-count",
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
    let set = |item: MagType| MagType::Set(Box::new(item));
    let map = |key: MagType, value: MagType| MagType::Map(Box::new(key), Box::new(value));
    let tag = |value: MagType| MagType::TypeTag(Box::new(value));
    let descriptor = MagType::TypeDescriptor;
    let packed = MagType::PackedValue;
    let mut signatures = match name {
        "__map-empty" => vec![
            function(vec![tag(var("key"))], map(var("key"), var("value"))),
            function(
                vec![tag(var("key")), tag(var("value"))],
                map(var("key"), var("value")),
            ),
        ],
        "__map-insert" => vec![function(
            vec![map(var("key"), var("value")), var("key"), var("value")],
            map(var("key"), var("value")),
        )],
        "__map-put" => vec![function(
            vec![map(var("key"), var("value")), var("key"), var("value")],
            map(var("key"), var("value")),
        )],
        "__map-get-or" => vec![function(
            vec![map(var("key"), var("value")), var("key"), var("value")],
            var("value"),
        )],
        "__map-get" => vec![function(
            vec![map(var("key"), var("value")), var("key")],
            var("value"),
        )],
        "__map-contains" => vec![function(
            vec![map(var("key"), var("value")), var("key")],
            MagType::Bool,
        )],
        "__map-count" => vec![function(vec![map(var("key"), var("value"))], MagType::Int)],
        "__map-union-left" => vec![function(
            vec![map(var("key"), var("value")), map(var("key"), var("value"))],
            map(var("key"), var("value")),
        )],
        "__set-empty" => vec![function(vec![tag(var("item"))], set(var("item")))],
        "__set-insert" => vec![function(
            vec![set(var("item")), var("item")],
            set(var("item")),
        )],
        "__set-contains" => vec![function(vec![set(var("item")), var("item")], MagType::Bool)],
        "__set-count" => vec![function(vec![set(var("item"))], MagType::Int)],
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
            vec![MagType::Record(BTreeMap::new())],
            list(MagType::String),
        )],
        "get" => vec![function(
            vec![MagType::Record(BTreeMap::new()), MagType::String],
            var("field"),
        )],
        "assoc" => vec![function(
            vec![
                MagType::Record(BTreeMap::new()),
                MagType::String,
                var("field"),
            ],
            MagType::Record(BTreeMap::new()),
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
        if name == "or" && params.len() == 2 && params[0] == params[1] {
            signatures.push(function(params.clone(), params[0].clone()));
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

// A function's constraints must survive value transport (records, conditionals,
// returned callbacks), not only a direct call through its original name.
fn check_equality_specialization(
    env: &Env,
    expression: &CheckedExpr,
    expected: &MagType,
    outer: &HashMap<String, MagType>,
) -> Result<(), MagError> {
    let mut substitutions = outer.clone();
    let original = substitute(&expression.ty, outer);
    let expected = substitute(expected, outer);
    let mut bindable = HashSet::new();
    collect_vars(&original, &mut bindable);
    // This mapping is local evidence propagation; ordinary checking has already
    // established compatibility and owns diagnostics for mismatched types.
    let _ = compatible_with_bindable(env, &expected, &original, &mut substitutions, &bindable);
    let resolved = substitute(&expression.ty, &substitutions);
    match &expression.kind {
        CheckedExprKind::BindingRef(id) => {
            for requirement in resolved_binding_requirements(env, *id, &resolved) {
                require_equality_admissible(env, &substitute(&requirement, &substitutions))?;
            }
        }
        CheckedExprKind::Function(function) => {
            for parameter in &function.equality_params {
                require_equality_admissible(
                    env,
                    &substitute(&MagType::Var(parameter.clone()), &substitutions),
                )?;
            }
            for binding in &function.body.bindings {
                check_equality_specialization(
                    env,
                    &binding.initializer,
                    &binding.ty,
                    &substitutions,
                )?;
            }
            for expression in &function.body.expressions {
                check_equality_specialization(env, expression, &expression.ty, &substitutions)?;
            }
        }
        CheckedExprKind::Call { callee, args } => {
            let signature = substitute(&callee.ty, &substitutions);
            check_equality_specialization(env, callee, &signature, &substitutions)?;
            if let MagType::Function(params, _) = signature {
                for (argument, parameter) in args.iter().zip(params) {
                    check_equality_specialization(env, argument, &parameter, &substitutions)?;
                }
            }
        }
        CheckedExprKind::Vector(items) => {
            for (index, item) in items.iter().enumerate() {
                let expected = match &resolved {
                    MagType::List(item) => item.as_ref(),
                    MagType::Product(items) => items.get(index).unwrap_or(&item.ty),
                    _ => &item.ty,
                };
                check_equality_specialization(env, item, expected, &substitutions)?;
            }
        }
        CheckedExprKind::Map(fields) => {
            for (name, field) in fields {
                let expected = match &resolved {
                    MagType::Record(fields) => fields.get(name).unwrap_or(&field.ty),
                    _ => &field.ty,
                };
                check_equality_specialization(env, field, expected, &substitutions)?;
            }
        }
        CheckedExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            check_equality_specialization(env, condition, &MagType::Bool, &substitutions)?;
            check_equality_specialization(env, then_branch, &resolved, &substitutions)?;
            check_equality_specialization(env, else_branch, &resolved, &substitutions)?;
        }
        CheckedExprKind::Ascribe { value, .. } => {
            check_equality_specialization(env, value, &resolved, &substitutions)?
        }
        CheckedExprKind::Construct { payload, .. } => {
            check_equality_specialization(env, payload, &payload.ty, &substitutions)?
        }
        CheckedExprKind::Match { value, arms } => {
            check_equality_specialization(env, value, &value.ty, &substitutions)?;
            for arm in arms {
                check_equality_specialization(env, &arm.body, &resolved, &substitutions)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn collect_called_bindings(expression: &CheckedExpr, calls: &mut Vec<(BindingId, MagType)>) {
    match &expression.kind {
        CheckedExprKind::Call { callee, args } => {
            if let CheckedExprKind::BindingRef(id) = callee.kind {
                calls.push((id, callee.ty.clone()));
            }
            collect_called_bindings(callee, calls);
            for argument in args {
                collect_called_bindings(argument, calls);
            }
        }
        CheckedExprKind::Vector(items) => {
            for item in items {
                collect_called_bindings(item, calls);
            }
        }
        CheckedExprKind::Map(fields) => {
            for (_, value) in fields {
                collect_called_bindings(value, calls);
            }
        }
        CheckedExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            collect_called_bindings(condition, calls);
            collect_called_bindings(then_branch, calls);
            collect_called_bindings(else_branch, calls);
        }
        CheckedExprKind::Construct { payload, .. }
        | CheckedExprKind::Ascribe { value: payload, .. } => {
            collect_called_bindings(payload, calls);
        }
        CheckedExprKind::Match { value, arms } => {
            collect_called_bindings(value, calls);
            for arm in arms {
                collect_called_bindings(&arm.body, calls);
            }
        }
        CheckedExprKind::Function(function) => {
            collect_called_bindings_in_block(&function.body, calls)
        }
        CheckedExprKind::Unit
        | CheckedExprKind::Str(_)
        | CheckedExprKind::Int(_)
        | CheckedExprKind::Float(_)
        | CheckedExprKind::Bool(_)
        | CheckedExprKind::Keyword(_)
        | CheckedExprKind::BindingRef(_)
        | CheckedExprKind::TypeTag(_) => {}
    }
}

fn collect_called_bindings_in_block(block: &CheckedBlock, calls: &mut Vec<(BindingId, MagType)>) {
    for binding in &block.bindings {
        collect_called_bindings(&binding.initializer, calls);
    }
    for expression in &block.expressions {
        collect_called_bindings(expression, calls);
    }
}

fn resolved_binding_requirements(env: &Env, id: BindingId, resolved: &MagType) -> Vec<MagType> {
    let mut requirements = env.equality_requirements(id);
    if requirements.is_empty() {
        if let Ok(Value::Fn(function)) = env.ready_binding(id) {
            requirements = function
                .equality_params
                .iter()
                .cloned()
                .map(MagType::Var)
                .collect();
        }
    }
    let Some(original) = env.binding_metadata(id).and_then(|metadata| metadata.ty) else {
        return requirements;
    };
    let mut variables = HashSet::new();
    collect_vars(&original, &mut variables);
    let mut substitution = HashMap::new();
    if compatible_with_bindable(env, resolved, &original, &mut substitution, &variables).is_err() {
        return requirements;
    }
    requirements
        .iter()
        .map(|requirement| substitute(requirement, &substitution))
        .collect()
}

fn finalize_block_equality_requirements(
    env: &Env,
    bindings: &mut [CheckedBinding],
) -> Result<(), MagError> {
    let mut changed = true;
    while changed {
        changed = false;
        for binding in bindings.iter() {
            let CheckedExprKind::Function(function) = &binding.initializer.kind else {
                let (_, variables) = with_equality_obligation_scope(|| {
                    check_equality_specialization(
                        env,
                        &binding.initializer,
                        &binding.ty,
                        &HashMap::new(),
                    )
                })?;
                let mut requirements = variables.into_iter().map(MagType::Var).collect::<Vec<_>>();
                requirements.sort_by_key(ToString::to_string);
                if requirements != env.equality_requirements(binding.id) {
                    env.set_equality_requirements(binding.id, requirements);
                    changed = true;
                }
                continue;
            };
            let mut required = env
                .equality_requirements(binding.id)
                .into_iter()
                .collect::<HashSet<_>>();
            let (_, variables) = with_equality_obligation_scope(|| {
                check_equality_specialization(
                    env,
                    &binding.initializer,
                    &binding.ty,
                    &HashMap::new(),
                )
            })?;
            required.extend(
                variables
                    .into_iter()
                    .filter(|name| function.type_params.contains(name))
                    .map(MagType::Var),
            );
            let mut calls = Vec::new();
            collect_called_bindings_in_block(&function.body, &mut calls);
            for (callee, resolved) in calls {
                for requirement in resolved_binding_requirements(env, callee, &resolved) {
                    let mut variables = HashSet::new();
                    equality_admissible_in(env, &requirement, &mut variables, &mut HashSet::new())
                        .map_err(MagError::Type)?;
                    required.extend(variables.into_iter().map(MagType::Var));
                }
            }
            let mut required = required.into_iter().collect::<Vec<_>>();
            required.sort_by_key(ToString::to_string);
            if required != env.equality_requirements(binding.id) {
                env.set_equality_requirements(binding.id, required);
                changed = true;
            }
        }
    }
    for binding in bindings {
        let CheckedExprKind::Function(function) = &binding.initializer.kind else {
            continue;
        };
        let mut updated = function.as_ref().clone();
        updated.equality_params = env
            .equality_requirements(binding.id)
            .into_iter()
            .filter_map(|requirement| match requirement {
                MagType::Var(name) if updated.type_params.contains(&name) => Some(name),
                _ => None,
            })
            .collect();
        binding.initializer = Arc::new(checked(
            binding.ty.clone(),
            CheckedExprKind::Function(Arc::new(updated)),
        ));
    }
    Ok(())
}

/// Resolves a source block into typed expressions whose authored references
/// point at stable binding identities. Evaluation never has to repeat name or
/// overload resolution.
pub fn compile_block(env: &Env, expressions: &[BlockItem]) -> Result<CheckedBlock, MagError> {
    compile_block_in(env, &[], expressions, None)
}

fn compile_block_in(
    env: &Env,
    outer: &[CheckedScope],
    expressions: &[BlockItem],
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
            let inferred = if let Expr::Name(source) = initializer {
                let candidates = visible_candidates(env, outer, &current, source);
                match candidates.as_slice() {
                    [candidate] if !candidate.generic_binders.is_empty() => {
                        Ok(instantiate_candidate(candidate).0)
                    }
                    _ => infer_shape(env, &mut types, &scoped_type_vars, initializer),
                }
            } else {
                infer_shape(env, &mut types, &scoped_type_vars, initializer)
            };
            match inferred {
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
        let checked = if let Expr::Function(function) = initializer {
            compile_function(
                env,
                &scopes,
                Some(name),
                function,
                Some(&candidate.ty),
                Some(id),
            )?
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
    finalize_block_equality_requirements(env, &mut bindings)?;

    let last_expression = expressions
        .iter()
        .rposition(|item| matches!(item, BlockItem::Expr(_)));
    let mut checked_expressions = Vec::new();
    for (index, item) in expressions.iter().enumerate() {
        if let Some(expression) = block_expr(item)? {
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

#[allow(clippy::too_many_arguments)]
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
    _scoped_type_vars: &HashSet<String>,
    expression: &Expr,
) -> Result<MagType, MagError> {
    infer(env, locals, expression)
}

fn compile_expr(
    env: &Env,
    scopes: &[CheckedScope],
    expression: &Expr,
    expected: Option<&MagType>,
) -> Result<CheckedExpr, MagError> {
    let value = match expression {
        Expr::Unit => checked(MagType::Unit, CheckedExprKind::Unit),
        Expr::Bool(value) => checked(MagType::Bool, CheckedExprKind::Bool(*value)),
        Expr::Int(value) => checked(MagType::Int, CheckedExprKind::Int(*value)),
        Expr::Float(value) => checked(MagType::Float, CheckedExprKind::Float(*value)),
        Expr::Str(value) => checked(MagType::String, CheckedExprKind::Str(value.clone())),
        Expr::Keyword(value) => checked(MagType::String, CheckedExprKind::Keyword(value.clone())),
        Expr::Name(name) => compile_symbol(env, scopes, name, expected)?,
        Expr::Vector(items) => compile_vector(env, scopes, items, expected)?,
        Expr::Record(fields) => compile_map(env, scopes, fields, expected)?,
        Expr::If {
            condition,
            then_branch,
            else_branch,
        } => compile_if(env, scopes, condition, then_branch, else_branch, expected)?,
        Expr::Construct {
            owner,
            constructor,
            payload,
        } => compile_construct(env, scopes, owner, constructor, payload)?,
        Expr::Match { value, arms } => compile_match(env, scopes, value, arms, expected)?,
        Expr::Ascribe { target, value } => compile_ascribe(env, scopes, target, value)?,
        Expr::TypeTag(target) => compile_type_tag(env, scopes, target)?,
        Expr::Function(function) => compile_function(env, scopes, None, function, expected, None)?,
        Expr::Call { callee, args } => compile_call(env, scopes, callee, args, expected)?,
        Expr::Invalid(error) => return Err(error.clone().into_mag_error()),
    };
    if let Some(expected) = expected {
        compatible_static(env, &value.ty, expected, &mut HashMap::new()).map_err(MagError::Type)?;
    }
    check_equality_specialization(env, &value, expected.unwrap_or(&value.ty), &HashMap::new())?;
    env.profile_counters(|counters| {
        counters.checked_expressions = counters.checked_expressions.saturating_add(1);
    });
    Ok(value)
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
    let mut substitution = HashMap::new();
    for item in items {
        let item_expected = expected_item.map(|ty| substitute(ty, &substitution));
        let value = compile_expr(env, scopes, item, item_expected.as_ref())?;
        if let Some(ty) = &item_expected {
            // Element constraints must survive the contextual list type: callers
            // infer their generic arguments from the vector's resulting type.
            compatible_static(env, &value.ty, ty, &mut substitution).map_err(MagError::Type)?;
            item_type = expected_item.map(|ty| substitute(ty, &substitution));
        } else if let Some(current) = &item_type {
            compatible_static(env, &value.ty, current, &mut HashMap::new()).map_err(|_| {
                MagError::Type(format!(
                    "list elements must have one compatible type, got {current} and {}",
                    value.ty
                ))
            })?;
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

fn compile_map(
    env: &Env,
    scopes: &[CheckedScope],
    fields: &[(String, Expr)],
    expected: Option<&MagType>,
) -> Result<CheckedExpr, MagError> {
    let mut checked_fields = Vec::with_capacity(fields.len());
    let mut field_types = BTreeMap::new();
    for (key, value) in fields {
        let key = key.clone();
        let expected_field = match expected {
            Some(MagType::Record(fields)) => fields.get(&key),
            _ => None,
        };
        let value = compile_expr(env, scopes, value, expected_field)?;
        if field_types.insert(key.clone(), value.ty.clone()).is_some() {
            return Err(MagError::Type(format!("duplicate record field {key}")));
        }
        checked_fields.push((key, value));
    }
    Ok(checked(
        MagType::Record(field_types),
        CheckedExprKind::Map(checked_fields),
    ))
}

fn compile_call(
    env: &Env,
    scopes: &[CheckedScope],
    callee_expression: &Expr,
    expressions: &[Expr],
    expected: Option<&MagType>,
) -> Result<CheckedExpr, MagError> {
    if let Expr::Name(name) = callee_expression {
        let candidates = all_candidates(env, scopes, name);
        let function_candidates = candidates
            .into_iter()
            .filter(|candidate| matches!(candidate.ty, MagType::Function(_, _)))
            .collect::<Vec<_>>();
        if !function_candidates.is_empty() {
            let user_call = compile_overloaded_call(
                env,
                scopes,
                name,
                &function_candidates,
                expressions,
                expected,
            );
            if user_call.is_ok() || builtin_id(env, name).is_none() {
                return user_call;
            }
        }
        if let Some(id) = builtin_id(env, name) {
            return compile_builtin_call(env, scopes, name, id, expressions, expected);
        }
    }
    let callee = compile_expr(env, scopes, callee_expression, None)?;
    let MagType::Function(params, result) = &callee.ty else {
        return Err(MagError::Type(format!("cannot call {}", callee.ty)));
    };
    if params.len() != expressions.len() {
        return Err(MagError::Arity {
            expected: params.len(),
            got: expressions.len(),
        });
    }
    let mut substitution = HashMap::new();
    let mut args = Vec::with_capacity(params.len());
    for (expression, parameter) in expressions.iter().zip(params) {
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

fn compile_construct(
    env: &Env,
    scopes: &[CheckedScope],
    authored_owner: &Type,
    constructor_name: &str,
    payload_expression: &Expr,
) -> Result<CheckedExpr, MagError> {
    let owner = parse_checked_type(env, scopes, authored_owner)?;
    let (constructor, payload_type) = instantiated_constructor(env, &owner, constructor_name)?;
    let payload = compile_expr(env, scopes, payload_expression, Some(&payload_type))?;
    Ok(checked(
        owner.clone(),
        CheckedExprKind::Construct {
            owner,
            constructor,
            payload: Box::new(payload),
        },
    ))
}

fn compile_match(
    env: &Env,
    scopes: &[CheckedScope],
    value_expression: &Expr,
    arm_expressions: &[crate::authored::MatchArm],
    expected: Option<&MagType>,
) -> Result<CheckedExpr, MagError> {
    let value = compile_expr(env, scopes, value_expression, None)?;
    let constructors = adt_constructors(env, &value.ty)?;
    let mut seen = HashSet::new();
    let mut arms = Vec::with_capacity(arm_expressions.len());
    let mut result = expected.cloned();
    for arm_expression in arm_expressions {
        let (constructor, payload_type) =
            instantiated_constructor(env, &value.ty, &arm_expression.constructor)?;
        if !seen.insert(constructor.clone()) {
            return Err(MagError::Type(format!(
                "duplicate match arm for {}",
                arm_expression.constructor
            )));
        }
        let binding_name = &arm_expression.binding;
        let binding_id = env.allocate_binding_id(binding_name, Some(payload_type.clone()));
        let mut arm_scope = CheckedScope::new();
        insert_checked_candidate(
            env,
            scopes,
            &mut arm_scope,
            binding_name,
            CheckedCandidate {
                id: binding_id,
                ty: payload_type.clone(),
                generic_binders: vec![],
                contributes_type_vars: false,
            },
        )?;
        let mut arm_scopes = scopes.to_vec();
        arm_scopes.push(arm_scope);
        let body = compile_expr(env, &arm_scopes, &arm_expression.body, result.as_ref())?;
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
                name: binding_name.clone(),
                ty: payload_type,
            },
            body: Box::new(body),
        });
    }
    ensure_adt_exhaustive(&constructors, &seen)?;
    Ok(checked(
        result.unwrap_or(MagType::Unit),
        CheckedExprKind::Match {
            value: Box::new(value),
            arms,
        },
    ))
}

fn adt_constructors(env: &Env, owner: &MagType) -> Result<Vec<ConstructorDecl>, MagError> {
    let MagType::Named(name, arguments) = owner else {
        return Err(MagError::Type(format!(
            "match expects an ADT value, got {owner}"
        )));
    };
    let declaration = env
        .type_decl(name)
        .ok_or_else(|| MagError::Type(format!("unknown nominal type {name}")))?;
    if declaration.params.len() != arguments.len() {
        return Err(MagError::Type(format!(
            "{name} expects {} type arguments, got {}",
            declaration.params.len(),
            arguments.len()
        )));
    }
    let TypeDeclBody::Adt(constructors) = declaration.body else {
        return Err(MagError::Type(format!(
            "match expects an ADT value, got {owner}"
        )));
    };
    let substitutions = declaration
        .params
        .iter()
        .cloned()
        .zip(arguments.iter().cloned())
        .collect::<HashMap<_, _>>();
    Ok(constructors
        .into_iter()
        .map(|constructor| ConstructorDecl {
            id: constructor.id,
            payload: substitute(&constructor.payload, &substitutions),
        })
        .collect())
}

fn instantiated_constructor(
    env: &Env,
    owner: &MagType,
    name: &str,
) -> Result<(ConstructorDeclarationId, MagType), MagError> {
    let constructors = adt_constructors(env, owner)?;
    constructors
        .into_iter()
        .find(|constructor| constructor.id.name == name)
        .map(|constructor| (constructor.id, constructor.payload))
        .ok_or_else(|| MagError::Type(format!("constructor {name} is not a member of {owner}")))
}

fn ensure_adt_exhaustive(
    constructors: &[ConstructorDecl],
    seen: &HashSet<ConstructorDeclarationId>,
) -> Result<(), MagError> {
    let missing = constructors
        .iter()
        .filter(|constructor| !seen.contains(&constructor.id))
        .map(|constructor| constructor.id.name.clone())
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

fn compile_if(
    env: &Env,
    scopes: &[CheckedScope],
    condition: &Expr,
    then_expression: &Expr,
    else_expression: &Expr,
    expected: Option<&MagType>,
) -> Result<CheckedExpr, MagError> {
    let condition = compile_expr(env, scopes, condition, Some(&MagType::Bool))?;
    let then_branch = compile_expr(env, scopes, then_expression, expected)?;
    let else_branch = compile_expr(env, scopes, else_expression, expected)?;
    compatible_static(env, &then_branch.ty, &else_branch.ty, &mut HashMap::new()).map_err(
        |_| {
            MagError::Type(format!(
                "if branches must return one compatible type, got {} and {}",
                then_branch.ty, else_branch.ty
            ))
        },
    )?;
    let ty = else_branch.ty.clone();
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
    authored_target: &Type,
    source: &Expr,
) -> Result<CheckedExpr, MagError> {
    let target = parse_checked_type(env, scopes, authored_target)?;
    let source_expected = match source {
        Expr::Name(name) if all_candidates(env, scopes, name).len() > 1 => Some(&target),
        _ => None,
    };
    let value = match source {
        Expr::Vector(values) if matches!(target, MagType::Product(_)) => {
            compile_vector(env, scopes, values, Some(&target))?
        }
        Expr::Call { callee, args } => compile_call(env, scopes, callee, args, Some(&target))?,
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
    authored_target: &Type,
) -> Result<CheckedExpr, MagError> {
    let target = parse_checked_type(env, scopes, authored_target)?;
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
        validate_equality_requirements(
            env,
            &instantiated_equality_requirements(env, candidate),
            &substitution,
        )?;
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
            if validate_equality_requirements(
                env,
                &instantiated_equality_requirements(env, candidate),
                &substitution,
            )
            .is_err()
            {
                continue;
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
    for (argument, parameter) in args.iter().zip(params) {
        check_equality_specialization(
            env,
            argument,
            &substitute(parameter, &substitution),
            &substitution,
        )?;
    }
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
    if name == "__map-empty" && expressions.len() == 1 {
        let Some(MagType::Map(key_type, _)) = expected else {
            return Err(MagError::Type(
                "one-argument __map-empty requires an expected Map type".into(),
            ));
        };
        require_equality_admissible(env, key_type)?;
        let key = compile_expr(
            env,
            scopes,
            &expressions[0],
            Some(&MagType::TypeTag(key_type.clone())),
        )?;
        let output = expected.cloned().unwrap_or_else(|| unreachable!());
        return Ok(checked(
            output.clone(),
            CheckedExprKind::Call {
                callee: Box::new(checked(
                    MagType::Function(vec![key.ty.clone()], Box::new(output.clone())),
                    CheckedExprKind::BindingRef(id),
                )),
                args: vec![key],
            },
        ));
    }
    if name == "get" {
        if expressions.len() != 2 {
            return Err(MagError::Arity {
                expected: 2,
                got: expressions.len(),
            });
        }
        let target = compile_expr(env, scopes, &expressions[0], None)?;
        if matches!(target.ty, MagType::HostInputs | MagType::Map(_, _)) {
            return Err(MagError::Type(format!(
                "get expects a record, got {}",
                target.ty
            )));
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
    body: Vec<BlockItem>,
}

fn parse_function(
    env: &Env,
    scopes: &[CheckedScope],
    function: &Function,
) -> Result<ParsedFunction, MagError> {
    let mut vars = visible_type_variables(scopes);
    vars.extend(function.type_params.iter().cloned());
    Ok(ParsedFunction {
        type_params: function.type_params.clone(),
        params: function
            .params
            .iter()
            .map(|parameter| {
                Ok((
                    parameter.name.clone(),
                    resolve_type(env, &parameter.ty, &vars)?,
                ))
            })
            .collect::<Result<Vec<_>, MagError>>()?,
        result: resolve_type(env, &function.result, &vars)?,
        body: function.body.clone(),
    })
}

fn compile_function(
    env: &Env,
    scopes: &[CheckedScope],
    name: Option<&str>,
    authored: &Function,
    expected: Option<&MagType>,
    binding_id: Option<BindingId>,
) -> Result<CheckedExpr, MagError> {
    let function = parse_function(env, scopes, authored)?;
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
    let (body, equality_variables) = with_equality_obligation_scope(|| {
        compile_block_in(env, &body_scopes, &function.body, Some(&function.result))
    })?;
    record_equality_variables(
        equality_variables
            .iter()
            .filter(|variable| !function.type_params.contains(variable))
            .cloned(),
    );
    let equality_params = function
        .type_params
        .iter()
        .filter(|parameter| equality_variables.contains(*parameter))
        .cloned()
        .collect::<Vec<_>>();
    if let Some(id) = binding_id {
        env.set_equality_requirements(
            id,
            equality_params.iter().cloned().map(MagType::Var).collect(),
        );
    }
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
            equality_params,
            params: checked_params,
            result: function.result,
            body: Arc::new(body),
        })),
    ))
}

fn parse_checked_type(
    env: &Env,
    scopes: &[CheckedScope],
    expression: &Type,
) -> Result<MagType, MagError> {
    resolve_type(env, expression, &visible_type_variables(scopes))
}

pub(crate) fn resolve_type(
    env: &Env,
    authored: &Type,
    vars: &HashSet<String>,
) -> Result<MagType, MagError> {
    match authored {
        Type::Name(name) if vars.contains(name) => Ok(MagType::Var(name.clone())),
        Type::Name(name) => {
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
        Type::Record(fields) => {
            let mut resolved = BTreeMap::new();
            for (name, ty) in fields {
                if resolved
                    .insert(name.clone(), resolve_type(env, ty, vars)?)
                    .is_some()
                {
                    return Err(MagError::Type(format!(
                        "duplicate record type field {name}"
                    )));
                }
            }
            Ok(MagType::Record(resolved))
        }
        Type::Product(types) => types
            .iter()
            .map(|ty| resolve_type(env, ty, vars))
            .collect::<Result<Vec<_>, _>>()
            .map(MagType::Product),
        Type::Tag(ty) => Ok(MagType::TypeTag(Box::new(resolve_type(env, ty, vars)?))),
        Type::Function { params, result } => Ok(MagType::Function(
            params
                .iter()
                .map(|ty| resolve_type(env, ty, vars))
                .collect::<Result<Vec<_>, _>>()?,
            Box::new(resolve_type(env, result, vars)?),
        )),
        Type::Apply {
            constructor,
            arguments,
        } => {
            let declaration = match env.lookup(constructor)? {
                Value::TypeDecl(declaration) => declaration,
                _ => {
                    return Err(MagError::Type(format!(
                        "{constructor} is not a declared type"
                    )))
                }
            };
            if declaration.params.len() != arguments.len() {
                return Err(MagError::Type(format!(
                    "{} expects {} type arguments, got {}",
                    declaration.name,
                    declaration.params.len(),
                    arguments.len()
                )));
            }
            let arguments = arguments
                .iter()
                .map(|ty| resolve_type(env, ty, vars))
                .collect::<Result<Vec<_>, _>>()?;
            match (declaration.body, arguments.as_slice()) {
                (TypeDeclBody::Native, [item]) if declaration.name == "core.List" => {
                    Ok(MagType::List(Box::new(item.clone())))
                }
                (TypeDeclBody::Native, [key, value]) if declaration.name == "core.Map" => {
                    Ok(MagType::Map(Box::new(key.clone()), Box::new(value.clone())))
                }
                (TypeDeclBody::Native, [item]) if declaration.name == "core.Set" => {
                    Ok(MagType::Set(Box::new(item.clone())))
                }
                (TypeDeclBody::Native, _) => Err(MagError::Type(format!(
                    "unsupported native type application {}",
                    declaration.name
                ))),
                _ => Ok(MagType::Named(declaration.name, arguments)),
            }
        }
        Type::Invalid(message) => Err(MagError::Type(message.clone())),
    }
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
        MagType::Named(_, arguments) | MagType::Product(arguments) => {
            arguments.iter().any(contains_union)
        }
        MagType::TypeTag(value) | MagType::List(value) | MagType::Set(value) => {
            contains_union(value)
        }
        MagType::Map(key, value) => contains_union(key) || contains_union(value),
        MagType::Record(fields) => fields.values().any(contains_union),
        MagType::Function(parameters, result) => {
            parameters.iter().any(contains_union) || contains_union(result)
        }
        _ => false,
    }
}

fn instantiated_equality_requirements(env: &Env, candidate: &CheckedCandidate) -> Vec<MagType> {
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
    let mut requirements = env.equality_requirements(candidate.id);
    if requirements.is_empty() {
        if let Ok(Value::Fn(function)) = env.ready_binding(candidate.id) {
            requirements = function
                .equality_params
                .iter()
                .cloned()
                .map(MagType::Var)
                .collect();
        }
    }
    requirements
        .iter()
        .map(|requirement| substitute(requirement, &substitutions))
        .collect()
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
            MagType::Set(item) => MagType::Set(Box::new(canonicalize(item, variables, next))),
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
        Value::List(v) if v.is_empty() => Some(MagType::EmptyList),
        Value::List(v) => Some(MagType::List(Box::new(v.first().and_then(value_type)?))),
        Value::Product(v) => Some(MagType::Product(
            v.iter().map(value_type).collect::<Option<Vec<_>>>()?,
        )),
        Value::Record(fields) => Some(MagType::Record(
            fields
                .iter()
                .map(|(key, value)| Some((key.clone(), value_type(value)?)))
                .collect::<Option<BTreeMap<_, _>>>()?,
        )),
        Value::Map(entries) => {
            let (key, value) = entries.first()?;
            Some(MagType::Map(
                Box::new(value_type(key)?),
                Box::new(value_type(value)?),
            ))
        }
        Value::Set(items) => Some(MagType::Set(Box::new(value_type(items.first()?)?))),
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
        Value::Adt { owner, .. } => Some(owner.clone()),
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
        MagType::Record(fields) => key.and_then(|k| fields.get(k).cloned()),
        MagType::Named(name, args) => env.type_decl(name).and_then(|decl| {
            let substitutions = decl
                .params
                .iter()
                .cloned()
                .zip(args.iter().cloned())
                .collect();
            let TypeDeclBody::Nominal(body) = decl.body else {
                return None;
            };
            let body = substitute(&body, &substitutions);
            field_type(env, &body, key)
        }),
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
    if actual == expected {
        return Ok(());
    }
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
    match expected {
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
        MagType::Set(e) => match actual {
            MagType::Set(a) => compatible_in(env, a, e, subst, bindable),
            _ => Err(format!("expected {expected}, got {actual}")),
        },
        MagType::Map(ek, ev) => match actual {
            MagType::Map(ak, av) => {
                compatible_in(env, ak, ek, subst, bindable)?;
                compatible_in(env, av, ev, subst, bindable)
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
        MagType::Set(t) => MagType::Set(Box::new(substitute(t, subst))),
        MagType::Map(k, v) => MagType::Map(
            Box::new(substitute(k, subst)),
            Box::new(substitute(v, subst)),
        ),
        MagType::Record(f) => MagType::Record(
            f.iter()
                .map(|(k, v)| (k.clone(), substitute(v, subst)))
                .collect(),
        ),
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
        MagType::Named(_, args) | MagType::Product(args) => {
            for arg in args {
                collect_vars(arg, out);
            }
        }
        MagType::List(item) | MagType::Set(item) => collect_vars(item, out),
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
