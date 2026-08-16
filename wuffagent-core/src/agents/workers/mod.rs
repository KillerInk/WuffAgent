pub mod generic;
pub mod research;
pub mod coding;
pub mod implementation;
pub mod general;

// GenericWorker is re-exported from worker.rs
pub use generic::ExecutingWorker;
pub use research::ResearchWorker;
pub use coding::CodingWorker;
pub use implementation::ImplementationWorker;
pub use general::GeneralWorker;
