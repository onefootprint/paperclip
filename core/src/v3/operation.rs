use super::{v2, Either, OperationEitherResponse};

impl From<v2::Operation<v2::DefaultParameterRaw, v2::DefaultResponseRaw>> for openapiv3::Operation {
    fn from(v2: v2::Operation<v2::DefaultParameterRaw, v2::DefaultResponseRaw>) -> Self {
        let mut request_body: Option<openapiv3::RequestBody> = None;
        let mut form_data: Option<openapiv3::AnySchema> = None;
        let v2v = v2.clone();

        let parameters = v2
            .parameters
            .iter()
            .filter_map(|p| match p {
                Either::Left(reference) => Some(reference.into()),
                Either::Right(parameter) => {
                    let either: Either<
                        openapiv3::Parameter,
                        Either<openapiv3::RequestBody, Option<openapiv3::ReferenceOr<openapiv3::Schema>>>,
                    > = parameter.clone().into();
                    match either {
                        Either::Right(r) => match r {
                            Either::Left(l) => {
                                request_body = Some(l);
                                None
                            }
                            Either::Right(Some(schema)) => {
                                let boxed_item = match schema {
                                    openapiv3::ReferenceOr::Item(schema) => openapiv3::ReferenceOr::Item(Box::new(schema)),
                                    openapiv3::ReferenceOr::Reference{ reference } => openapiv3::ReferenceOr::Reference{ reference },
                                };
                                if let Some(any) = form_data.as_mut() {
                                    any.properties.insert(
                                        parameter.name.clone(),
                                        boxed_item,
                                    );
                                } else {
                                    let mut any = openapiv3::AnySchema::default();
                                    if parameter.required {
                                        any.required.push(parameter.name.clone());
                                    }
                                    any.properties.insert(
                                        parameter.name.clone(),
                                        boxed_item,
                                    );
                                    form_data = Some(any);
                                }
                                None
                            }
                            Either::Right(None) => None,
                        },
                        Either::Left(parameter) => Some(openapiv3::ReferenceOr::Item(parameter)),
                    }
                }
            })
            .collect();

        let request_body = if let Some(request_body) = request_body {
            Some(openapiv3::ReferenceOr::Item(request_body))
        } else if let Some(form_data) = form_data {
            let mut request_body = openapiv3::RequestBody::default();
            match v2.consumes {
                None => None,
                Some(consumes) => {
                    for media in consumes {
                        request_body.content.insert(media.0.to_string(), {
                            openapiv3::MediaType {
                                schema: Some(openapiv3::ReferenceOr::Item(openapiv3::Schema {
                                    schema_data: Default::default(),
                                    schema_kind: openapiv3::SchemaKind::Any(form_data.clone()),
                                })),
                                ..Default::default()
                            }
                        });
                    }

                    Some(openapiv3::ReferenceOr::Item(request_body))
                }
            }
        } else {
            None
        };

        openapiv3::Operation {
            tags: v2.tags,
            summary: v2.summary,
            description: v2.description,
            external_docs: None,
            operation_id: v2.operation_id,
            parameters,
            request_body,
            responses: openapiv3::Responses {
                default: None,
                responses: v2
                    .responses
                    .iter()
                    .fold(indexmap::IndexMap::new(), |mut i, (k, v)| {
                        if let Ok(code) = k.parse::<u16>() {
                            let code = openapiv3::StatusCode::Code(code);
                            i.insert(
                                code,
                                OperationEitherResponse {
                                    operation: &v2v,
                                    response: v,
                                }
                                .into(),
                            );
                        }
                        i
                    }),
                ..Default::default()
            },
            deprecated: v2.deprecated,
            security: if v2.security.is_empty() {
                None
            } else {
                Some(
                    v2.security
                        .iter()
                        .map(|s| {
                            s.iter().fold(indexmap::IndexMap::new(), |mut i, (k, v)| {
                                i.insert(k.to_string(), v.clone());
                                i
                            })
                        })
                        .collect(),
                )
            },
            servers: vec![],
            extensions: indexmap::IndexMap::new(),
        }
    }
}
