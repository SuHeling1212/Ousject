#[derive(Debug)]
pub struct VirtualMachine {
    manager: Arc<InMemoryObjectManager>,
    context: AccessContext,
    console_provider: Option<ObjectId>,
    console_driver: Option<Arc<dyn ConsoleProvider>>,
    kernel_services: BTreeMap<String, ObjectId>,
    providers: Arc<ProviderRegistry>,
    program_cache: Arc<Mutex<ProgramCache>>,
    package_verification_cache: Arc<Mutex<PackageVerificationCache>>,
    process_reaper: Arc<Mutex<Option<mpsc::Sender<ProcessReaperMessage>>>>,
}
