use crate::ast::Value;
use crate::checker::substitute;
use crate::env::Env;
use crate::error::MagError;
use crate::types::MagType;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

pub(crate) fn json_number_to_i64(number: &serde_json::Number) -> Option<i64> {
    number.as_i64().or_else(|| {
        number
            .as_f64()
            .filter(|value| {
                value.fract() == 0.0
                    && *value >= i64::MIN as f64
                    && *value < 9_223_372_036_854_775_808.0
            })
            .map(|value| value as i64)
    })
}

pub fn value_to_json(env: &Env, value: &Value) -> Result<serde_json::Value, MagError> {
    match value {
        Value::Unit => Ok(serde_json::Value::Null),
        Value::Str(v) => Ok(v.clone().into()),
        Value::Int(v) => Ok((*v).into()),
        Value::Float(v) if v.is_finite() => Ok(serde_json::json!(v)),
        Value::Float(v) => Err(MagError::Eval(format!(
            "cannot serialize non-finite Float {v} to JSON"
        ))),
        Value::Bool(v) => Ok((*v).into()),
        Value::Keyword(v) => Ok(format!(":{v}").into()),
        Value::Symbol(v) => Ok(v.clone().into()),
        Value::TypeTag(ty) => concrete_type_to_json(ty),
        Value::TypeDescriptor(ty) => concrete_type_to_json(ty),
        Value::TypeSchema(schema) => serde_json::to_value(schema)
            .map_err(|e| MagError::Eval(format!("serialize type schema: {e}"))),
        Value::SemanticTypeId(id) => Ok(id.as_str().into()),
        Value::PackedValue(value) => Ok(serde_json::json!({
            "$mag": "packed-value",
            "value": value_to_json(env, value)?,
        })),
        Value::JsonValue(value) => Ok(canonicalize_json(value.clone())),
        Value::HostInputs(_) => Err(MagError::Eval(
            "compiler host inputs cannot enter an artifact".into(),
        )),
        Value::List(v) | Value::Product(v) => Ok(serde_json::Value::Array(
            v.iter()
                .map(|value| value_to_json(env, value))
                .collect::<Result<_, _>>()?,
        )),
        Value::Set(v) => {
            let mut items = v
                .iter()
                .map(|value| value_to_json(env, value))
                .collect::<Result<Vec<_>, _>>()?;
            sort_canonical_json(&mut items);
            Ok(serde_json::json!({"$mag": "set", "items": items}))
        }
        Value::Adt {
            constructor,
            payload,
            ..
        } => Ok(serde_json::json!({
            "constructor": constructor.name,
            "value": value_to_json(env, payload)?,
        })),
        Value::Record(v) => Ok(serde_json::Value::Object(
            v.iter()
                .map(|(key, value)| Ok((key.clone(), value_to_json(env, value)?)))
                .collect::<Result<_, MagError>>()?,
        )),
        Value::Map(entries) => map_to_json(
            env,
            entries,
            !entries.is_empty() && entries.iter().all(|(key, _)| matches!(key, Value::Str(_))),
        ),
        Value::Artifact(v) => Ok(v.clone()),
        Value::Typed(value, ty) => {
            if let MagType::Map(key, _) = ty {
                let mut unwrapped = value.as_ref();
                while let Value::Typed(inner, _) = unwrapped {
                    unwrapped = inner;
                }
                if let Value::Map(entries) = unwrapped {
                    return map_to_json(env, entries, key.as_ref() == &MagType::String);
                }
            }
            value_to_json(env, value)
        }
        other => Err(MagError::Eval(format!(
            "cannot serialize {} to JSON",
            other.type_name()
        ))),
    }
}

fn map_to_json(
    env: &Env,
    entries: &[(Value, Value)],
    string_keys: bool,
) -> Result<serde_json::Value, MagError> {
    if string_keys {
        let object = entries
            .iter()
            .map(|(key, value)| {
                let key = key
                    .as_str()
                    .ok_or_else(|| MagError::Type("Map String key is not a String".into()))?;
                Ok((key.to_owned(), value_to_json(env, value)?))
            })
            .collect::<Result<BTreeMap<_, _>, MagError>>()?;
        Ok(serde_json::Value::Object(object.into_iter().collect()))
    } else {
        let mut entries = entries
            .iter()
            .map(|(key, value)| {
                Ok(serde_json::json!([
                    value_to_json(env, key)?,
                    value_to_json(env, value)?
                ]))
            })
            .collect::<Result<Vec<_>, MagError>>()?;
        sort_canonical_json(&mut entries);
        Ok(serde_json::json!({"$mag":"map", "entries":entries}))
    }
}

fn canonicalize_json(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(canonicalize_json).collect())
        }
        serde_json::Value::Object(fields) => serde_json::Value::Object(
            fields
                .into_iter()
                .map(|(key, value)| (key, canonicalize_json(value)))
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .collect(),
        ),
        scalar => scalar,
    }
}

fn canonical_json_key(value: &serde_json::Value) -> String {
    serde_json::to_string(value)
        .unwrap_or_else(|_| unreachable!("JSON value serialization is infallible"))
}

fn sort_canonical_json(values: &mut [serde_json::Value]) {
    values.sort_by_key(canonical_json_key);
}

fn exact_envelope<'a>(
    value: &'a serde_json::Value,
    tag: &str,
    payload: &str,
    ty: &MagType,
) -> Result<&'a serde_json::Map<String, serde_json::Value>, MagError> {
    let object = value
        .as_object()
        .ok_or_else(|| MagError::Type(format!("expected {tag} envelope for {ty}")))?;
    if object.len() != 2
        || object.get("$mag").and_then(serde_json::Value::as_str) != Some(tag)
        || !object.contains_key(payload)
    {
        return Err(MagError::Type(format!(
            "expected exact {tag} envelope {{$mag, {payload}}} for {ty}"
        )));
    }
    Ok(object)
}

fn reject_duplicate_values(env: &Env, values: &[Value], label: &str) -> Result<(), MagError> {
    for (index, value) in values.iter().enumerate() {
        if values[..index]
            .iter()
            .any(|previous| crate::eval::equal(env, previous, value))
        {
            return Err(MagError::Type(format!(
                "duplicate {label} at position {index}"
            )));
        }
    }
    Ok(())
}

fn reject_duplicate_map_keys(env: &Env, entries: &[(Value, Value)]) -> Result<(), MagError> {
    for (index, (key, _)) in entries.iter().enumerate() {
        if entries[..index]
            .iter()
            .any(|(previous, _)| crate::eval::equal(env, previous, key))
        {
            return Err(MagError::Type(format!(
                "duplicate map key at position {index}"
            )));
        }
    }
    Ok(())
}

pub fn concrete_type_to_json(
    ty: &crate::types::ConcreteType,
) -> Result<serde_json::Value, MagError> {
    use crate::types::ConcreteType;
    let primitive = |name: &str| serde_json::json!({ "kind": "primitive", "name": name });
    Ok(match ty {
        ConcreteType::JsonValue => primitive("JsonValue"),
        ConcreteType::Unit => primitive("Unit"),
        ConcreteType::Bool => primitive("Bool"),
        ConcreteType::Int => primitive("Int"),
        ConcreteType::Float => primitive("Float"),
        ConcreteType::String => primitive("String"),
        ConcreteType::Named {
            name,
            arguments,
            body,
        } => serde_json::json!({
            "kind": "named",
            "name": name,
            "arguments": arguments.iter().map(concrete_type_to_json).collect::<Result<Vec<_>, _>>()?,
            "body": concrete_type_to_json(body)?,
        }),
        ConcreteType::Adt {
            name,
            arguments,
            constructors,
        } => serde_json::json!({
            "kind": "adt",
            "name": name,
            "arguments": arguments.iter().map(concrete_type_to_json).collect::<Result<Vec<_>, _>>()?,
            "constructors": constructors.iter().map(|constructor| Ok(serde_json::json!({
                "name": constructor.name,
                "payload": concrete_type_to_json(&constructor.payload)?,
            }))).collect::<Result<Vec<_>, MagError>>()?,
        }),
        ConcreteType::List { item } => serde_json::json!({
            "kind": "list", "item": concrete_type_to_json(item)?
        }),
        ConcreteType::Set { item } => serde_json::json!({
            "kind": "set", "item": concrete_type_to_json(item)?
        }),
        ConcreteType::Map { key, value } => serde_json::json!({
            "kind": "map",
            "key": concrete_type_to_json(key)?,
            "value": concrete_type_to_json(value)?,
        }),
        ConcreteType::Record { fields } => serde_json::json!({
            "kind": "record",
            "fields": fields.iter().map(|(name, ty)| Ok(serde_json::json!({
                "name": name, "type": concrete_type_to_json(ty)?
            }))).collect::<Result<Vec<_>, MagError>>()?,
        }),
        ConcreteType::Product { items } => serde_json::json!({
            "kind": "product",
            "items": items.iter().map(concrete_type_to_json).collect::<Result<Vec<_>, _>>()?,
        }),
    })
}

/// Decode the canonical descriptor representation emitted into MAG artifacts.
/// Runtime callers use this before applying the compiler's compatibility
/// relation, so arbitrary Lua tables never become semantic authority.
pub fn concrete_type_from_json(
    value: &serde_json::Value,
) -> Result<crate::types::ConcreteType, MagError> {
    use crate::types::ConcreteType;

    let object = value
        .as_object()
        .ok_or_else(|| MagError::Type("semantic descriptor must be an object".into()))?;
    let kind = string_field(object, "kind")?;
    let descriptor = match kind {
        "primitive" => match string_field(object, "name")? {
            "JsonValue" => ConcreteType::JsonValue,
            "Unit" => ConcreteType::Unit,
            "Bool" => ConcreteType::Bool,
            "Int" => ConcreteType::Int,
            "Float" => ConcreteType::Float,
            "String" => ConcreteType::String,
            name => {
                return Err(MagError::Type(format!(
                    "unknown semantic primitive {name:?}"
                )))
            }
        },
        "named" => ConcreteType::Named {
            name: string_field(object, "name")?.to_owned(),
            arguments: descriptor_list(object, "arguments")?,
            // Compiler artifacts include the body. Separately-owned Lua
            // declarations and direct kernel fixtures name nominal
            // constructors without embedding MAG definitions; those nodes
            // are usable for nominal compatibility but not stable identity.
            body: Box::new(match object.get("body") {
                Some(body) => concrete_type_from_json(body)?,
                None => ConcreteType::Unit,
            }),
        },
        "adt" => {
            let constructors = object
                .get("constructors")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| {
                    MagError::Type("ADT semantic descriptor needs constructors".into())
                })?;
            let mut previous: Option<&str> = None;
            let constructors = constructors
                .iter()
                .map(|constructor| {
                    let constructor = constructor.as_object().ok_or_else(|| {
                        MagError::Type("ADT constructor descriptor must be an object".into())
                    })?;
                    let name = string_field(constructor, "name")?;
                    if previous.is_some_and(|previous| previous >= name) {
                        return Err(MagError::Type(
                            "ADT constructors must be uniquely sorted by name".into(),
                        ));
                    }
                    previous = Some(name);
                    Ok(crate::types::ConcreteConstructor {
                        name: name.to_owned(),
                        payload: concrete_type_from_json(constructor.get("payload").ok_or_else(
                            || MagError::Type("ADT constructor descriptor needs payload".into()),
                        )?)?,
                    })
                })
                .collect::<Result<Vec<_>, MagError>>()?;
            if constructors.is_empty() {
                return Err(MagError::Type("ADT descriptor needs constructors".into()));
            }
            ConcreteType::Adt {
                name: string_field(object, "name")?.to_owned(),
                arguments: descriptor_list(object, "arguments")?,
                constructors,
            }
        }
        "list" => ConcreteType::List {
            item: Box::new(concrete_type_from_json(object.get("item").ok_or_else(
                || MagError::Type("list semantic descriptor needs item".into()),
            )?)?),
        },
        "set" => ConcreteType::Set {
            item: Box::new(concrete_type_from_json(object.get("item").ok_or_else(
                || MagError::Type("set semantic descriptor needs item".into()),
            )?)?),
        },
        "map" => ConcreteType::Map {
            key: Box::new(concrete_type_from_json(object.get("key").ok_or_else(
                || MagError::Type("map semantic descriptor needs key".into()),
            )?)?),
            value: Box::new(concrete_type_from_json(object.get("value").ok_or_else(
                || MagError::Type("map semantic descriptor needs value".into()),
            )?)?),
        },
        "record" => {
            let fields = object
                .get("fields")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| MagError::Type("record semantic descriptor needs fields".into()))?;
            let mut decoded = BTreeMap::new();
            let mut previous: Option<&str> = None;
            for field in fields {
                let field = field.as_object().ok_or_else(|| {
                    MagError::Type("record descriptor field must be an object".into())
                })?;
                let name = string_field(field, "name")?;
                if previous.is_some_and(|previous| previous >= name) {
                    return Err(MagError::Type(
                        "record descriptor fields must be uniquely sorted".into(),
                    ));
                }
                previous = Some(name);
                let ty =
                    concrete_type_from_json(field.get("type").ok_or_else(|| {
                        MagError::Type("record descriptor field needs type".into())
                    })?)?;
                decoded.insert(name.to_owned(), ty);
            }
            ConcreteType::Record { fields: decoded }
        }
        "union" => {
            return Err(MagError::Type(
                "structural union semantic descriptors are unsupported; declare an ADT".into(),
            ));
        }
        "product" => {
            let items = descriptor_list(object, "items")?;
            if items.len() < 2 {
                return Err(MagError::Type(
                    "product semantic descriptor needs at least two items".into(),
                ));
            }
            ConcreteType::Product { items }
        }
        other => {
            return Err(MagError::Type(format!(
                "unknown semantic descriptor kind {other:?}"
            )))
        }
    };
    let canonical = concrete_type_to_json(&descriptor)?;
    if canonical != fill_missing_named_bodies(value) {
        return Err(MagError::Type(
            "semantic descriptor is not in canonical compiler form".into(),
        ));
    }
    Ok(descriptor)
}

fn fill_missing_named_bodies(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.iter().map(fill_missing_named_bodies).collect())
        }
        serde_json::Value::Object(object) => {
            let mut object = object
                .iter()
                .map(|(key, value)| (key.clone(), fill_missing_named_bodies(value)))
                .collect::<serde_json::Map<_, _>>();
            if object.get("kind").and_then(serde_json::Value::as_str) == Some("named")
                && !object.contains_key("body")
            {
                object.insert(
                    "body".into(),
                    serde_json::json!({"kind":"primitive","name":"Unit"}),
                );
            }
            serde_json::Value::Object(object)
        }
        scalar => scalar.clone(),
    }
}

fn string_field<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<&'a str, MagError> {
    object
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| MagError::Type(format!("semantic descriptor needs string {field}")))
}

fn descriptor_list(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Vec<crate::types::ConcreteType>, MagError> {
    object
        .get(field)
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| MagError::Type(format!("semantic descriptor needs list {field}")))?
        .iter()
        .map(concrete_type_from_json)
        .collect()
}

pub fn json_to_value(value: &serde_json::Value) -> Value {
    match value {
        serde_json::Value::Null => Value::Unit,
        serde_json::Value::Bool(v) => Value::Bool(*v),
        serde_json::Value::Number(v) => v
            .as_i64()
            .map(Value::Int)
            .or_else(|| v.as_f64().map(Value::Float))
            .unwrap_or_else(|| Value::Str(v.to_string())),
        serde_json::Value::String(v) => Value::Str(v.clone()),
        serde_json::Value::Array(v) => Value::List(Arc::new(v.iter().map(json_to_value).collect())),
        serde_json::Value::Object(v) => Value::Record(Arc::new(
            v.iter()
                .map(|(k, v)| (k.clone(), json_to_value(v)))
                .collect(),
        )),
    }
}

#[derive(Clone, Copy)]
enum RecordDecode {
    Exact,
    Projection,
}

pub fn json_to_typed_value(
    env: &Env,
    value: &serde_json::Value,
    ty: &MagType,
) -> Result<Value, MagError> {
    decode_typed_value(env, value, ty, RecordDecode::Exact)
}

pub(crate) fn project_typed_value(
    env: &Env,
    value: &serde_json::Value,
    ty: &MagType,
) -> Result<Value, MagError> {
    decode_typed_value(env, value, ty, RecordDecode::Projection)
}

fn decode_typed_value(
    env: &Env,
    value: &serde_json::Value,
    ty: &MagType,
    mode: RecordDecode,
) -> Result<Value, MagError> {
    let decoded = match ty {
        MagType::Named(name, args) => {
            let decl = env
                .type_decl(name)
                .ok_or_else(|| MagError::Type(format!("unknown nominal type {name}")))?;
            let substitutions: HashMap<_, _> = decl
                .params
                .iter()
                .cloned()
                .zip(args.iter().cloned())
                .collect();
            match decl.body {
                crate::ast::TypeDeclBody::Nominal(body) => Value::Typed(
                    std::sync::Arc::new(decode_typed_value(
                        env,
                        value,
                        &substitute(&body, &substitutions),
                        mode,
                    )?),
                    ty.clone(),
                ),
                crate::ast::TypeDeclBody::Adt(constructors) => {
                    let object = value
                        .as_object()
                        .ok_or_else(|| MagError::Type(format!("expected ADT envelope for {ty}")))?;
                    if object.len() != 2
                        || !object.contains_key("constructor")
                        || !object.contains_key("value")
                    {
                        return Err(MagError::Type(format!(
                            "expected exact ADT envelope {{constructor, value}} for {ty}"
                        )));
                    }
                    let constructor_name = object
                        .get("constructor")
                        .and_then(serde_json::Value::as_str)
                        .ok_or_else(|| MagError::Type("ADT constructor must be a string".into()))?;
                    let constructor = constructors
                        .into_iter()
                        .find(|constructor| constructor.id.name == constructor_name)
                        .ok_or_else(|| {
                            MagError::Type(format!(
                                "constructor {constructor_name} is not a member of {ty}"
                            ))
                        })?;
                    let payload_type = substitute(&constructor.payload, &substitutions);
                    Value::Adt {
                        owner: ty.clone(),
                        constructor: constructor.id,
                        payload: std::sync::Arc::new(decode_typed_value(
                            env,
                            object
                                .get("value")
                                .ok_or_else(|| MagError::Type("ADT envelope needs value".into()))?,
                            &payload_type,
                            mode,
                        )?),
                    }
                }
                crate::ast::TypeDeclBody::Native => {
                    return Err(MagError::Type(format!(
                        "native type {name} cannot be decoded as a nominal value"
                    )))
                }
            }
        }
        MagType::List(item) => Value::List(Arc::new(
            value
                .as_array()
                .ok_or_else(|| MagError::Type(format!("expected {ty}")))?
                .iter()
                .map(|entry| decode_typed_value(env, entry, item, mode))
                .collect::<Result<_, _>>()?,
        )),
        MagType::Set(item) => {
            let object = exact_envelope(value, "set", "items", ty)?;
            let values = object
                .get("items")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| MagError::Type(format!("set items must be an array for {ty}")))?;
            let decoded = values
                .iter()
                .map(|entry| decode_typed_value(env, entry, item, mode))
                .collect::<Result<Vec<_>, _>>()?;
            reject_duplicate_values(env, &decoded, "set item")?;
            Value::Set(Arc::new(decoded))
        }
        MagType::Map(key, item) if key.as_ref() == &MagType::String => Value::Map(Arc::new(
            value
                .as_object()
                .ok_or_else(|| MagError::Type(format!("expected {ty}")))?
                .iter()
                .map(|(key, entry)| {
                    Ok((
                        Value::Str(key.clone()),
                        decode_typed_value(env, entry, item, mode)?,
                    ))
                })
                .collect::<Result<Vec<_>, MagError>>()?,
        )),
        MagType::Map(key, item) => {
            let object = exact_envelope(value, "map", "entries", ty)?;
            let entries = object
                .get("entries")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| MagError::Type(format!("map entries must be an array for {ty}")))?;
            let decoded = entries
                .iter()
                .map(|entry| {
                    let pair =
                        entry
                            .as_array()
                            .filter(|pair| pair.len() == 2)
                            .ok_or_else(|| {
                                MagError::Type(format!("map entry must be [key, value] for {ty}"))
                            })?;
                    Ok((
                        decode_typed_value(env, &pair[0], key, mode)?,
                        decode_typed_value(env, &pair[1], item, mode)?,
                    ))
                })
                .collect::<Result<Vec<_>, MagError>>()?;
            reject_duplicate_map_keys(env, &decoded)?;
            Value::Map(Arc::new(decoded))
        }
        MagType::Record(fields) => {
            let object = value
                .as_object()
                .ok_or_else(|| MagError::Type(format!("expected {ty}")))?;
            if matches!(mode, RecordDecode::Exact) && object.len() != fields.len() {
                return Err(MagError::Type(format!(
                    "expected exact record fields for {ty}"
                )));
            }
            Value::Record(Arc::new(
                fields
                    .iter()
                    .map(|(key, field_type)| {
                        let field = object.get(key).ok_or_else(|| {
                            MagError::Type(format!("missing field {key} for {ty}"))
                        })?;
                        Ok((
                            key.clone(),
                            decode_typed_value(env, field, field_type, mode)?,
                        ))
                    })
                    .collect::<Result<BTreeMap<_, _>, MagError>>()?,
            ))
        }
        MagType::Product(components) => {
            let values = value
                .as_array()
                .ok_or_else(|| MagError::Type(format!("expected ordered tuple {ty}")))?;
            if values.len() != components.len() {
                return Err(MagError::Type(format!(
                    "expected {} tuple positions for {ty}, got {}",
                    components.len(),
                    values.len()
                )));
            }
            Value::Product(std::sync::Arc::new(
                values
                    .iter()
                    .zip(components)
                    .map(|(position, component)| decode_typed_value(env, position, component, mode))
                    .collect::<Result<_, _>>()?,
            ))
        }
        MagType::JsonValue => Value::JsonValue(value.clone()),
        MagType::Float => Value::Float(
            value
                .as_f64()
                .ok_or_else(|| MagError::Type(format!("expected {ty}")))?,
        ),
        MagType::Int => Value::Int(
            value
                .as_number()
                .and_then(json_number_to_i64)
                .ok_or_else(|| MagError::Type(format!("expected {ty}")))?,
        ),
        MagType::Unit => {
            if !value.is_null() {
                return Err(MagError::Type(format!("expected {ty}")));
            }
            Value::Unit
        }
        MagType::Bool => Value::Bool(
            value
                .as_bool()
                .ok_or_else(|| MagError::Type(format!("expected {ty}")))?,
        ),
        MagType::String => Value::Str(
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| MagError::Type(format!("expected {ty}")))?,
        ),
        unsupported => {
            return Err(MagError::Type(format!(
                "{unsupported} is not representable as typed runtime JSON"
            )))
        }
    };
    Ok(if matches!(ty, MagType::Map(_, _) | MagType::Set(_)) {
        Value::Typed(Arc::new(decoded), ty.clone())
    } else {
        decoded
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ConcreteType;

    #[test]
    fn typed_int_accepts_provider_integer_notation_and_rejects_non_ints() {
        let env = Env::new();
        assert!(matches!(
            json_to_typed_value(&env, &serde_json::json!(1.0), &MagType::Int).unwrap(),
            Value::Int(1)
        ));
        for value in [
            serde_json::json!(1.5),
            serde_json::from_str::<serde_json::Value>("9223372036854775808.0").unwrap(),
        ] {
            assert!(json_to_typed_value(&env, &value, &MagType::Int).is_err());
        }
    }

    #[test]
    fn typed_int_canonicalizes_provider_integer_notation_recursively() {
        let env = Env::new();
        let ty = MagType::List(Box::new(MagType::Record(BTreeMap::from([(
            "value".into(),
            MagType::Int,
        )]))));
        let value = serde_json::json!([{"value": 1.0}]);
        let decoded = json_to_typed_value(&env, &value, &ty).unwrap();
        let Value::List(items) = decoded else {
            panic!("expected typed list");
        };
        let Value::Record(fields) = &items[0] else {
            panic!("expected typed record");
        };
        assert!(matches!(fields.get("value"), Some(Value::Int(1))));
    }

    #[test]
    fn typed_primitives_reject_wrong_json_shapes_recursively() {
        let env = Env::new();
        for (value, ty) in [
            (serde_json::json!("true"), MagType::Bool),
            (serde_json::json!(true), MagType::String),
            (serde_json::json!({}), MagType::Unit),
        ] {
            assert!(json_to_typed_value(&env, &value, &ty).is_err());
        }

        let nested = MagType::List(Box::new(MagType::Record(BTreeMap::from([
            ("enabled".into(), MagType::Bool),
            ("label".into(), MagType::String),
            ("marker".into(), MagType::Unit),
        ]))));
        for value in [
            serde_json::json!([{"enabled":"true","label":"ok","marker":null}]),
            serde_json::json!([{"enabled":true,"label":7,"marker":null}]),
            serde_json::json!([{"enabled":true,"label":"ok","marker":false}]),
        ] {
            assert!(json_to_typed_value(&env, &value, &nested).is_err());
        }
    }

    #[test]
    fn json_values_are_canonicalized_recursively_at_artifact_serialization() {
        let env = Env::new();
        let first = serde_json::from_str(r#"{"z":{"b":2,"a":1},"a":0}"#).unwrap();
        let second = serde_json::from_str(r#"{"a":0,"z":{"a":1,"b":2}}"#).unwrap();
        let first = value_to_json(&env, &Value::JsonValue(first)).unwrap();
        let second = value_to_json(&env, &Value::JsonValue(second)).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.to_string(), r#"{"a":0,"z":{"a":1,"b":2}}"#);
    }

    #[test]
    fn non_finite_floats_are_rejected_before_json_serialization() {
        let env = Env::new();
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let error = value_to_json(&env, &Value::Float(value)).unwrap_err();
            assert!(error.to_string().contains("non-finite Float"));
        }
        assert_eq!(
            value_to_json(&env, &Value::Float(f64::MAX)).unwrap(),
            serde_json::json!(f64::MAX)
        );
    }

    #[test]
    fn canonical_semantic_descriptors_round_trip_and_reject_forged_shape() {
        let descriptor = ConcreteType::Named {
            name: "main.Payload".into(),
            arguments: vec![],
            body: Box::new(ConcreteType::Record {
                fields: BTreeMap::from([("value".into(), ConcreteType::Int)]),
            }),
        };
        let encoded = concrete_type_to_json(&descriptor).unwrap();
        assert_eq!(concrete_type_from_json(&encoded).unwrap(), descriptor);

        let mut forged = encoded;
        forged
            .as_object_mut()
            .unwrap()
            .insert("wire".into(), serde_json::json!("not-semantic"));
        assert!(concrete_type_from_json(&forged).is_err());
        let structural_union = serde_json::json!({
            "kind": "union",
            "items": [
                {"kind":"primitive","name":"Int"},
                {"kind":"primitive","name":"String"}
            ]
        });
        let error = concrete_type_from_json(&structural_union)
            .unwrap_err()
            .to_string();
        assert!(error.contains("structural union semantic descriptors are unsupported"));
    }

    #[test]
    fn collection_wire_forms_are_canonical_and_reject_duplicates() {
        let env = Env::new();
        let map = Value::Map(Arc::new(vec![
            (Value::Int(2), Value::Str("b".into())),
            (Value::Int(1), Value::Str("a".into())),
        ]));
        assert_eq!(
            value_to_json(&env, &map).unwrap(),
            serde_json::json!({"$mag":"map","entries":[[1,"a"],[2,"b"]]})
        );
        let set = Value::Set(Arc::new(vec![Value::Int(2), Value::Int(1)]));
        assert_eq!(
            value_to_json(&env, &set).unwrap(),
            serde_json::json!({"$mag":"set","items":[1,2]})
        );

        let duplicate_map = serde_json::json!({"$mag":"map","entries":[[1,"a"],[1,"b"]]});
        assert!(json_to_typed_value(
            &env,
            &duplicate_map,
            &MagType::Map(Box::new(MagType::Int), Box::new(MagType::String))
        )
        .is_err());
        let duplicate_set = serde_json::json!({"$mag":"set","items":[1,1]});
        assert!(
            json_to_typed_value(&env, &duplicate_set, &MagType::Set(Box::new(MagType::Int)))
                .is_err()
        );
    }

    #[test]
    fn typed_records_reject_extra_fields() {
        let ty = MagType::Record(BTreeMap::from([("value".into(), MagType::Int)]));
        assert!(json_to_typed_value(
            &Env::new(),
            &serde_json::json!({"value": 1, "extra": 2}),
            &ty,
        )
        .is_err());
    }
}
