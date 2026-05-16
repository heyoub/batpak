#![allow(clippy::panic)]

use netbat as nb;
use syncbat::{EffectClass, Module, OperationDescriptor};

const PING: OperationDescriptor = OperationDescriptor::new(
    "ping",
    EffectClass::Inspect,
    "schema.ping.input.v1",
    "schema.ping.output.v1",
    "receipt.ping.v1",
);

const HEALTH_CHECK: OperationDescriptor = OperationDescriptor::new(
    "health.check",
    EffectClass::Inspect,
    "schema.health.input.v1",
    "schema.health.output.v1",
    "receipt.health.v1",
);

fn expose(base_path: &str) -> nb::ServerModule {
    let module = Module::from_operations("health", [PING]).expect("module builds");
    nb::ServerModule::expose(module, base_path).expect("module exposes")
}

#[test]
fn exposure_normalizes_outer_slashes_without_hiding_internal_segments() {
    let root = expose("");
    let api = expose("/api/");
    let padded = expose("//api//");

    assert_eq!(root.routes()[0].path(), "/ping");
    assert_eq!(api.routes()[0].path(), "/api/ping");
    assert_eq!(padded.routes()[0].path(), "/api/ping");
}

#[test]
fn route_constructors_accept_stable_boundary_shapes() {
    let endpoint =
        nb::Endpoint::new("health.check", "/api/health.check").expect("endpoint validates");
    let route = nb::Route::new("CALL", endpoint).expect("route validates");

    assert_eq!(route.method(), "CALL");
    assert_eq!(route.operation_name(), "health.check");
    assert_eq!(route.path(), "/api/health.check");
}

#[test]
fn endpoint_rejects_bad_operation_names() {
    for name in [
        "",
        ".ping",
        "ping.",
        "ping..now",
        "ping/name",
        "ping?x",
        "ping x",
    ] {
        let err = match nb::Endpoint::new(name, "/api/ping") {
            Ok(_) => panic!("expected operation-name rejection for {name:?}"),
            Err(error) => error,
        };

        assert!(
            matches!(err, nb::RouteValidationError::InvalidOperationName { .. }),
            "wrong error for {name:?}: {err:?}"
        );
    }
}

#[test]
fn endpoint_rejects_bad_paths() {
    for path in [
        "",
        "/",
        "api/ping",
        "/api/",
        "/api//ping",
        "/api/../ping",
        "/api/./ping",
        "/api/ping?x",
        "/api/ping#x",
        "/api/ping x",
        "/api\\ping",
    ] {
        let err = match nb::Endpoint::new("ping", path) {
            Ok(_) => panic!("expected path rejection for {path:?}"),
            Err(error) => error,
        };

        assert!(
            matches!(err, nb::RouteValidationError::InvalidPath { .. }),
            "wrong error for {path:?}: {err:?}"
        );
    }
}

#[test]
fn route_rejects_bad_method_labels() {
    let endpoint = nb::Endpoint::new("ping", "/api/ping").expect("endpoint validates");

    for method in ["", "call", "CALL POST", "CALL/POST"] {
        let err = match nb::Route::new(method, endpoint.clone()) {
            Ok(_) => panic!("expected method rejection for {method:?}"),
            Err(error) => error,
        };

        assert!(
            matches!(err, nb::RouteValidationError::InvalidMethod { .. }),
            "wrong error for {method:?}: {err:?}"
        );
    }
}

#[test]
fn server_module_exposure_rejects_bad_base_paths() {
    for base_path in [
        "api//v1",
        "../api",
        "api/../v1",
        "api/./v1",
        "api?x",
        "api#x",
        "api v1",
        "api\\v1",
    ] {
        let module = Module::from_operations("health", [PING]).expect("module builds");
        let err = match nb::ServerModule::expose(module, base_path) {
            Ok(_) => panic!("expected base path rejection for {base_path:?}"),
            Err(error) => error,
        };

        assert!(
            matches!(err, nb::RouteValidationError::InvalidPath { .. }),
            "wrong error for {base_path:?}: {err:?}"
        );
    }
}

#[test]
fn server_rejects_duplicate_method_path_pairs_across_modules() {
    let first = Module::from_operations("health", [PING]).expect("module builds");
    let second = Module::from_operations("health_alt", [PING]).expect("module builds");
    let mut server = nb::Server::new();
    server
        .mount(nb::ServerModule::expose(first, "/api").expect("first exposes"))
        .expect("first mounts");

    let err = match server.mount(nb::ServerModule::expose(second, "/api").expect("second exposes"))
    {
        Ok(_) => panic!("expected duplicate route rejection"),
        Err(error) => error,
    };

    assert_eq!(
        err,
        nb::RouteValidationError::DuplicateRoute {
            method: "CALL",
            path: "/api/ping".to_owned(),
        }
    );
}

#[test]
fn server_accepts_distinct_routes_in_stable_mount_order() {
    let first = Module::from_operations("health", [PING]).expect("module builds");
    let second = Module::from_operations("health_extra", [HEALTH_CHECK]).expect("module builds");
    let mut server = nb::Server::new();

    server
        .mount(nb::ServerModule::expose(first, "/api").expect("first exposes"))
        .expect("first mounts")
        .mount(nb::ServerModule::expose(second, "/api").expect("second exposes"))
        .expect("second mounts");

    let paths = server.routes().map(nb::Route::path).collect::<Vec<_>>();
    assert_eq!(paths, vec!["/api/ping", "/api/health.check"]);
}
