// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::bedrock::request_metadata::METADATA_KEY_TYPE;
use crate::error::RoutingError;
use async_trait::async_trait;
use cm_rust::CapabilityTypeName;
use router_error::RouterError;
use sandbox::{Capability, Data, Request, Routable, Router};

pub trait WithPorcelainType {
    /// Returns a router that ensures the capability request has a porcelain
    /// type that is the same as the type of the capability returned by the
    /// router.
    fn with_porcelain_type(self, porcelain_type: CapabilityTypeName) -> Router;
}

impl WithPorcelainType for Router {
    fn with_porcelain_type(self, porcelain_type: CapabilityTypeName) -> Router {
        #[derive(Debug, Clone)]
        struct RouterWithPorcelainType {
            router: Router,
            porcelain_type: CapabilityTypeName,
        }

        #[async_trait]
        impl Routable for RouterWithPorcelainType {
            async fn route(&self, request: Request) -> Result<Capability, RouterError> {
                let RouterWithPorcelainType { router, porcelain_type } = self;
                let Capability::Data(Data::String(capability_type)) = request
                    .metadata
                    .get(&cm_types::Name::new(METADATA_KEY_TYPE).unwrap())
                    .map_err(|()| RoutingError::BedrockNotCloneable)?
                    .unwrap_or_else(|| {
                        panic!("missing capability type {porcelain_type} for request: {request:?}")
                    })
                else {
                    return Err(RoutingError::BedrockNotPresentInDictionary {
                        name: String::from("type"),
                    }
                    .into());
                };
                let porcelain_type = porcelain_type.to_string();
                if capability_type == porcelain_type {
                    router.route(request).await
                } else {
                    Err(RoutingError::BedrockWrongCapabilityType {
                        actual: capability_type,
                        expected: porcelain_type,
                    }
                    .into())
                }
            }
        }

        Router::new(RouterWithPorcelainType { router: self, porcelain_type })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bedrock::request_metadata::{protocol_metadata, METADATA_KEY_TYPE};
    use assert_matches::assert_matches;
    use cm_rust::Availability;
    use cm_types::Name;
    use router_error::{DowncastErrorForTest, RouterError};
    use sandbox::{Capability, Data, Dict, WeakInstanceToken};
    use std::sync::Arc;

    #[derive(Debug)]
    struct FakeInstanceToken {}

    impl FakeInstanceToken {
        fn new() -> WeakInstanceToken {
            WeakInstanceToken { inner: Arc::new(FakeInstanceToken {}) }
        }
    }

    impl sandbox::WeakInstanceTokenAny for FakeInstanceToken {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    #[fuchsia::test]
    async fn porcelain_type_good() {
        let source: Capability = Data::String("hello".to_string()).into();
        let base = Router::new(source);
        let proxy = base.with_porcelain_type(CapabilityTypeName::Protocol);
        let capability = proxy
            .route(Request {
                availability: Availability::Optional,
                target: FakeInstanceToken::new(),
                debug: false,
                metadata: protocol_metadata(),
            })
            .await
            .unwrap();
        let capability = match capability {
            Capability::Data(d) => d,
            c => panic!("Bad enum {:#?}", c),
        };
        assert_eq!(capability, Data::String("hello".to_string()));
    }

    #[fuchsia::test]
    async fn porcelain_type_bad() {
        let source: Capability = Data::String("hello".to_string()).into();
        let base = Router::new(source);
        let proxy = base.with_porcelain_type(CapabilityTypeName::Protocol);
        let metadata = Dict::new();
        metadata
            .insert(
                Name::new(METADATA_KEY_TYPE).unwrap(),
                Capability::Data(Data::String(String::from("directory"))),
            )
            .unwrap();
        let error = proxy
            .route(Request {
                availability: Availability::Optional,
                target: FakeInstanceToken::new(),
                debug: false,
                metadata,
            })
            .await
            .unwrap_err();
        assert_matches!(
            error,
            RouterError::NotFound(err)
            if matches!(
                err.downcast_for_test::<RoutingError>(),
                RoutingError::BedrockWrongCapabilityType { actual: _, expected: _},
            )
        );
    }
}