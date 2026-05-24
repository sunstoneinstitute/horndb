use arrow::array::UInt64Array;
use horndb_wcoj::batch::{BindingBatchBuilder, STANDARD_VECTOR_SIZE};
use horndb_wcoj::pattern::Var;

#[test]
fn standard_vector_size_is_2048() {
    assert_eq!(STANDARD_VECTOR_SIZE, 2048);
}

#[test]
fn builder_flushes_at_capacity() {
    let mut b = BindingBatchBuilder::new(vec![Var(0), Var(1)]);
    for i in 0..STANDARD_VECTOR_SIZE as u64 {
        assert!(
            b.push_row(&[i, i + 1000]).is_none(),
            "no flush before capacity"
        );
    }
    let batch = b.push_row(&[9999, 19999]).expect("flush at overflow");
    assert_eq!(batch.num_rows(), STANDARD_VECTOR_SIZE);
    assert_eq!(batch.num_columns(), 2);
    let col0 = batch
        .column(0)
        .as_any()
        .downcast_ref::<UInt64Array>()
        .unwrap();
    assert_eq!(col0.value(0), 0);
    assert_eq!(
        col0.value(STANDARD_VECTOR_SIZE - 1),
        STANDARD_VECTOR_SIZE as u64 - 1
    );
    // The overflow row is now the first row of the next batch.
    let final_batch = b.finish().unwrap();
    assert_eq!(final_batch.num_rows(), 1);
}

#[test]
fn finish_on_empty_builder_returns_none() {
    let mut b = BindingBatchBuilder::new(vec![Var(0)]);
    assert!(b.finish().is_none());
}
