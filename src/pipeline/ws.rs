/// Trait for receiving real-time transaction notifications via WebSocket.
pub trait TransactionStream: Send + Sync {
    /// Subscribe to transaction logs for a program.
    fn subscribe(
        &self,
        program_id: &str,
    ) -> impl std::future::Future<Output = Result<(), super::PipelineError>> + Send;
}
