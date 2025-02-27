use super::{invalid_referenceor, v2};
use std::ops::Deref;

impl From<v2::DefaultSchemaRaw> for openapiv3::ReferenceOr<Box<openapiv3::Schema>> {
    fn from(v2: v2::DefaultSchemaRaw) -> Self {
        let x: openapiv3::ReferenceOr<openapiv3::Schema> = v2.into();
        match x {
            openapiv3::ReferenceOr::Reference { reference } => {
                openapiv3::ReferenceOr::Reference { reference }
            }
            openapiv3::ReferenceOr::Item(item) => openapiv3::ReferenceOr::Item(Box::new(item)),
        }
    }
}

impl From<v2::DefaultSchemaRaw> for openapiv3::ReferenceOr<openapiv3::Schema> {
    fn from(v2: v2::DefaultSchemaRaw) -> Self {
        match v2.reference.clone() {
            Some(reference) => v2::Reference { reference }.into(),
            None => {
                let item = openapiv3::Schema {
                    schema_data: openapiv3::SchemaData {
                        nullable: false,
                        read_only: false,
                        write_only: false,
                        deprecated: false,
                        external_docs: None,
                        example: v2.example,
                        title: v2.title,
                        description: v2.description,
                        discriminator: None,
                        default: None,
                        extensions: v2.extensions,
                    },
                    schema_kind: {
                        if let Some(data_type) = v2.data_type {
                            v2_data_type_to_v3(
                                &data_type,
                                &v2.format,
                                &v2.enum_,
                                &v2.items,
                                &v2.properties,
                                &v2.required,
                            )
                        } else if !v2.any_of.is_empty() {
                            let any_of = (v2.any_of)
                                .into_iter()
                                .map(|v2| openapiv3::ReferenceOr::<openapiv3::Schema>::from(*v2))
                                .collect();
                            openapiv3::SchemaKind::AnyOf {
                                any_of,
                            }
                        } else if !v2.all_of.is_empty() {
                            let all_of = (v2.all_of)
                                .into_iter()
                                .map(|v2| openapiv3::ReferenceOr::<openapiv3::Schema>::from(*v2))
                                .collect();
                            openapiv3::SchemaKind::AllOf {
                                all_of,
                            }
                        } else if let Some(c) = v2.const_ {
                            match c {
                                serde_json::Value::String(s) => openapiv3::SchemaKind::Type(
                                    openapiv3::Type::String(openapiv3::StringType {
                                        enumeration: vec![Some(s)],
                                        ..Default::default()
                                    }),
                                ),
                                _ => openapiv3::SchemaKind::Type(openapiv3::Type::Object(
                                    openapiv3::ObjectType::default(),
                                )),
                            }
                        } else {
                            openapiv3::SchemaKind::Type(openapiv3::Type::Object(
                                openapiv3::ObjectType::default(),
                            ))
                        }
                    },
                };
                openapiv3::ReferenceOr::Item(item)
            }
        }
    }
}

// helper function to convert a v2 DataType to v3, with explicit types making it more
// rust-analyzer friendly as the DefaultSchemaRaw is autogenerated by a macro
fn v2_data_type_to_v3(
    data_type: &v2::DataType,
    format: &Option<v2::DataTypeFormat>,
    enum_: &[serde_json::Value],
    items: &Option<Box<v2::DefaultSchemaRaw>>,
    properties: &std::collections::BTreeMap<String, Box<v2::DefaultSchemaRaw>>,
    required: &std::collections::BTreeSet<String>,
) -> openapiv3::SchemaKind {
    match data_type {
        v2::DataType::Integer => {
            openapiv3::SchemaKind::Type(openapiv3::Type::Integer(openapiv3::IntegerType {
                format: match format {
                    None => openapiv3::VariantOrUnknownOrEmpty::Empty,
                    Some(format) => match format {
                        v2::DataTypeFormat::Int32 => openapiv3::VariantOrUnknownOrEmpty::Item(
                            openapiv3::IntegerFormat::Int32,
                        ),
                        v2::DataTypeFormat::Int64 => openapiv3::VariantOrUnknownOrEmpty::Item(
                            openapiv3::IntegerFormat::Int64,
                        ),
                        other => {
                            debug_assert!(false, "Invalid data type format: {:?}", other);
                            openapiv3::VariantOrUnknownOrEmpty::Empty
                        }
                    },
                },
                multiple_of: None,
                exclusive_minimum: false,
                exclusive_maximum: false,
                minimum: None,
                maximum: None,
                enumeration: enum_
                    .iter()
                    .cloned()
                    .map(|v| serde_json::from_value(v).unwrap_or_default())
                    .collect(),
            }))
        }
        v2::DataType::Number => {
            openapiv3::SchemaKind::Type(openapiv3::Type::Number(openapiv3::NumberType {
                format: match format {
                    None => openapiv3::VariantOrUnknownOrEmpty::Empty,
                    Some(format) => match format {
                        v2::DataTypeFormat::Float => openapiv3::VariantOrUnknownOrEmpty::Item(
                            openapiv3::NumberFormat::Float {},
                        ),
                        v2::DataTypeFormat::Double => openapiv3::VariantOrUnknownOrEmpty::Item(
                            openapiv3::NumberFormat::Double {},
                        ),
                        other => {
                            debug_assert!(false, "Invalid data type format: {:?}", other);
                            openapiv3::VariantOrUnknownOrEmpty::Empty
                        }
                    },
                },
                multiple_of: None,
                exclusive_minimum: false,
                exclusive_maximum: false,
                minimum: None,
                maximum: None,
                enumeration: enum_
                    .iter()
                    .cloned()
                    .map(|v| serde_json::from_value(v).unwrap_or_default())
                    .collect(),
            }))
        }
        v2::DataType::String => {
            openapiv3::SchemaKind::Type(openapiv3::Type::String(openapiv3::StringType {
                format: match format {
                    None => openapiv3::VariantOrUnknownOrEmpty::Empty,
                    Some(format) => match format {
                        v2::DataTypeFormat::Byte => {
                            openapiv3::VariantOrUnknownOrEmpty::Item(openapiv3::StringFormat::Byte)
                        }
                        v2::DataTypeFormat::Binary => openapiv3::VariantOrUnknownOrEmpty::Item(
                            openapiv3::StringFormat::Binary,
                        ),
                        v2::DataTypeFormat::Date => {
                            openapiv3::VariantOrUnknownOrEmpty::Item(openapiv3::StringFormat::Date)
                        }
                        v2::DataTypeFormat::DateTime => openapiv3::VariantOrUnknownOrEmpty::Item(
                            openapiv3::StringFormat::DateTime,
                        ),
                        v2::DataTypeFormat::Password => openapiv3::VariantOrUnknownOrEmpty::Item(
                            openapiv3::StringFormat::Password,
                        ),
                        v2::DataTypeFormat::Other => {
                            debug_assert!(false, "Invalid data type format: other");
                            openapiv3::VariantOrUnknownOrEmpty::Unknown(
                                v2::DataTypeFormat::Other.to_string(),
                            )
                        }
                        others => openapiv3::VariantOrUnknownOrEmpty::Unknown(others.to_string()),
                    },
                },
                pattern: None,
                enumeration: enum_
                    .iter()
                    .cloned()
                    .map(|v| serde_json::from_value(v).unwrap_or_default())
                    .collect(),
                min_length: None,
                max_length: None,
            }))
        }
        v2::DataType::Boolean => openapiv3::SchemaKind::Type(openapiv3::Type::Boolean {}),
        v2::DataType::Array => {
            openapiv3::SchemaKind::Type(openapiv3::Type::Array(openapiv3::ArrayType {
                items: items.as_ref().map(|items| items.deref().clone().into()),
                min_items: None,
                max_items: None,
                unique_items: false,
            }))
        }
        v2::DataType::Object => {
            openapiv3::SchemaKind::Type(openapiv3::Type::Object(openapiv3::ObjectType {
                properties: {
                    properties
                        .iter()
                        .fold(indexmap::IndexMap::new(), |mut i, b| {
                            i.insert(b.0.to_string(), b.1.deref().clone().into());
                            i
                        })
                },
                required: required.iter().cloned().collect::<Vec<_>>(),
                additional_properties: None,
                min_properties: None,
                max_properties: None,
            }))
        }
        v2::DataType::File => {
            openapiv3::SchemaKind::Type(openapiv3::Type::String(openapiv3::StringType {
                format: openapiv3::VariantOrUnknownOrEmpty::Item(openapiv3::StringFormat::Binary),
                ..Default::default()
            }))
        }
    }
}

impl From<v2::Items> for openapiv3::ReferenceOr<Box<openapiv3::Schema>> {
    fn from(v2: v2::Items) -> Self {
        let kind = match v2.data_type {
            None => {
                return invalid_referenceor("Invalid Item, should have a data type".into());
            }
            Some(data_type) => match data_type {
                v2::DataType::Integer => {
                    openapiv3::SchemaKind::Type(openapiv3::Type::Integer(openapiv3::IntegerType {
                        format: match &v2.format {
                            None => openapiv3::VariantOrUnknownOrEmpty::Empty,
                            Some(format) => match format {
                                v2::DataTypeFormat::Int32 => {
                                    openapiv3::VariantOrUnknownOrEmpty::Item(
                                        openapiv3::IntegerFormat::Int32,
                                    )
                                }
                                v2::DataTypeFormat::Int64 => {
                                    openapiv3::VariantOrUnknownOrEmpty::Item(
                                        openapiv3::IntegerFormat::Int64,
                                    )
                                }
                                other => {
                                    return invalid_referenceor(format!(
                                        "Invalid data type format: {:?}",
                                        other
                                    ));
                                }
                            },
                        },
                        multiple_of: v2.multiple_of.map(|v| v as i64),
                        exclusive_minimum: v2.exclusive_minimum.unwrap_or_default(),
                        exclusive_maximum: v2.exclusive_maximum.unwrap_or_default(),
                        minimum: v2.minimum.map(|v| v as i64),
                        maximum: v2.maximum.map(|v| v as i64),
                        enumeration: v2
                            .enum_
                            .iter()
                            .cloned()
                            .map(|v| serde_json::from_value(v).unwrap_or_default())
                            .collect(),
                    }))
                }
                v2::DataType::Number => {
                    openapiv3::SchemaKind::Type(openapiv3::Type::Number(openapiv3::NumberType {
                        format: match &v2.format {
                            None => openapiv3::VariantOrUnknownOrEmpty::Empty,
                            Some(format) => match format {
                                v2::DataTypeFormat::Float => {
                                    openapiv3::VariantOrUnknownOrEmpty::Item(
                                        openapiv3::NumberFormat::Float {},
                                    )
                                }
                                v2::DataTypeFormat::Double => {
                                    openapiv3::VariantOrUnknownOrEmpty::Item(
                                        openapiv3::NumberFormat::Double {},
                                    )
                                }
                                other => {
                                    return invalid_referenceor(format!(
                                        "Invalid data type format: {:?}",
                                        other
                                    ));
                                }
                            },
                        },
                        multiple_of: v2.multiple_of.map(From::from),
                        exclusive_minimum: v2.exclusive_minimum.unwrap_or_default(),
                        exclusive_maximum: v2.exclusive_maximum.unwrap_or_default(),
                        minimum: v2.minimum.map(From::from),
                        maximum: v2.maximum.map(From::from),
                        enumeration: v2
                            .enum_
                            .iter()
                            .cloned()
                            .map(|v| serde_json::from_value(v).unwrap_or_default())
                            .collect(),
                    }))
                }
                v2::DataType::String => {
                    openapiv3::SchemaKind::Type(openapiv3::Type::String(openapiv3::StringType {
                        format: match &v2.format {
                            None => openapiv3::VariantOrUnknownOrEmpty::Empty,
                            Some(format) => match format {
                                v2::DataTypeFormat::Byte => {
                                    openapiv3::VariantOrUnknownOrEmpty::Item(
                                        openapiv3::StringFormat::Byte,
                                    )
                                }
                                v2::DataTypeFormat::Binary => {
                                    openapiv3::VariantOrUnknownOrEmpty::Item(
                                        openapiv3::StringFormat::Binary,
                                    )
                                }
                                v2::DataTypeFormat::Date => {
                                    openapiv3::VariantOrUnknownOrEmpty::Item(
                                        openapiv3::StringFormat::Date,
                                    )
                                }
                                v2::DataTypeFormat::DateTime => {
                                    openapiv3::VariantOrUnknownOrEmpty::Item(
                                        openapiv3::StringFormat::DateTime,
                                    )
                                }
                                v2::DataTypeFormat::Password => {
                                    openapiv3::VariantOrUnknownOrEmpty::Item(
                                        openapiv3::StringFormat::Password,
                                    )
                                }
                                other => {
                                    return invalid_referenceor(format!(
                                        "Invalid data type format: {:?}",
                                        other
                                    ));
                                }
                            },
                        },
                        pattern: v2.pattern.clone(),
                        enumeration: v2
                            .enum_
                            .iter()
                            .cloned()
                            .map(|v| serde_json::from_value(v).unwrap_or_default())
                            .collect(),
                        min_length: v2.min_length.map(|v| v as usize),
                        max_length: v2.max_length.map(|v| v as usize),
                    }))
                }
                v2::DataType::Boolean => openapiv3::SchemaKind::Type(openapiv3::Type::Boolean {}),
                v2::DataType::Array => {
                    openapiv3::SchemaKind::Type(openapiv3::Type::Array(openapiv3::ArrayType {
                        items: v2.items.map(|items| items.deref().clone().into()),
                        min_items: v2.min_items.map(|v| v as usize),
                        max_items: v2.max_items.map(|v| v as usize),
                        unique_items: v2.unique_items.unwrap_or_default(),
                    }))
                }
                invalid => {
                    return invalid_referenceor(format!("Invalid Item data_type: {:?}", invalid))
                }
            },
        };

        openapiv3::ReferenceOr::Item(Box::new(openapiv3::Schema {
            schema_data: Default::default(),
            schema_kind: kind,
        }))
    }
}
