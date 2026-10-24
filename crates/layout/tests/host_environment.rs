use docparse_layout::OnnxBackend;
use ort::memory::AllocatorType;

/// Host settings stay intact while DocParse reuses its environment across independent session lifetimes.
#[test]
fn reuses_the_host_environment() {
    // A one-element float Add graph requires an output allocation but no downloaded weights.
    const MODEL: &[u8] = b"\x08\x08\x3a\x3c\x0a\x0e\x0a\x01\x78\x0a\x01\x78\x12\x01\x79\x22\x03\x41\x64\x64\x12\x08\x68\x6f\x73\x74\x2d\x65\x6e\x76\x5a\x0f\x0a\x01\x78\x12\x0a\x0a\x08\x08\x01\x12\x04\x0a\x02\x08\x01\x62\x0f\x0a\x01\x79\x12\x0a\x0a\x08\x08\x01\x12\x04\x0a\x02\x08\x01\x42\x02\x10\x0d";
    let threads = ort::environment::GlobalThreadPoolOptions::default()
        .with_intra_threads(2)
        .expect("host threads");
    let host = ort::init()
        .with_telemetry(false)
        .with_global_thread_pool(threads)
        .with_execution_providers([ort::ep::CPU::default()
            .with_arena_allocator(false)
            .build()])
        .build()
        .expect("host environment");
    let mut host_builder =
        Some(ort::session::Session::builder(&host).expect("host session"));
    OnnxBackend::use_environment(host.clone())
        .expect("register host environment");
    OnnxBackend::use_environment(host.clone())
        .expect("idempotent registration");
    assert_eq!(host.execution_providers().len(), 1);
    drop(host);
    // The host disables its CPU arena; DocParse keeps its own provider defaults without mutating that policy.
    for expected_allocator in [
        AllocatorType::Device,
        AllocatorType::Arena,
        AllocatorType::Arena,
    ] {
        let mut builder = match host_builder.take() {
            Some(builder) => builder,
            None => OnnxBackend::compiled()
                .cpu_builder()
                .expect("shared environment after idle gap"),
        };
        let mut session =
            builder.commit_from_memory(MODEL).expect("CPU session");
        let input = ort::value::Tensor::from_array(([1], vec![7.0_f32]))
            .expect("input");
        let mut outputs =
            session.run(ort::inputs![input]).expect("Add inference");
        let output = outputs
            .remove("y")
            .expect("named output")
            .downcast::<ort::value::TensorValueType<f32>>()
            .expect("float output");
        assert_eq!(
            output.try_extract_tensor::<f32>().expect("output data").1,
            &[14.0_f32]
        );
        assert_eq!(output.memory_info().allocator_type(), expected_allocator);
    }
}
