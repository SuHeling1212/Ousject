#[derive(Debug)]
pub struct VirtualMachine {
    manager: Arc<InMemoryObjectManager>,
    context: AccessContext,
    terminal_provider: Option<ObjectId>,
    terminal_driver: Option<Arc<dyn TerminalProvider>>,
    kernel_services: BTreeMap<String, ObjectId>,
    providers: Arc<ProviderRegistry>,
    program_cache: Arc<Mutex<ProgramCache>>,
    package_verification_cache: Arc<Mutex<PackageVerificationCache>>,
    process_reaper: Arc<Mutex<Option<mpsc::Sender<ProcessReaperMessage>>>>,
}
