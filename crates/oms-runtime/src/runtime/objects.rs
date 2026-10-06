#[derive(Debug, Clone)]
pub struct CreateSpec {
    pub type_name: String,
    pub value: Value,
    pub parent: Option<ObjectId>,
    pub links: BTreeMap<String, ObjectId>,
}

impl CreateSpec {
    #[must_use]
    pub fn new(type_name: impl Into<String>, value: Value) -> Self {
        Self {
            type_name: type_name.into(),
            value,
            parent: None,
            links: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn with_parent(mut self, parent: ObjectId) -> Self {
        self.parent = Some(parent);
        self
    }

    #[must_use]
    pub fn with_link(mut self, name: impl Into<String>, target: ObjectId) -> Self {
        self.links.insert(name.into(), target);
        self
    }
}

#[derive(Debug, Clone, Default)]
pub struct ObjectQuery {
    pub type_id: Option<TypeId>,
    pub parent: Option<ObjectId>,
    pub capability: Option<Capability>,
    pub domain_capability: Option<String>,
}

impl ObjectQuery {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            type_id: None,
            parent: None,
            capability: None,
            domain_capability: None,
        }
    }

    #[must_use]
    pub const fn with_type(mut self, type_id: TypeId) -> Self {
        self.type_id = Some(type_id);
        self
    }

    #[must_use]
    pub const fn with_parent(mut self, parent: ObjectId) -> Self {
        self.parent = Some(parent);
        self
    }

    #[must_use]
    pub const fn with_capability(mut self, capability: Capability) -> Self {
        self.capability = Some(capability);
        self
    }

    #[must_use]
    pub fn with_domain_capability(mut self, capability: impl Into<String>) -> Self {
        self.domain_capability = Some(capability.into());
        self
    }
}

#[derive(Debug, Clone)]
pub struct CreateObject {
    pub id: ObjectId,
    pub type_id: TypeId,
    pub parent: Option<ObjectId>,
    pub state: Vec<u8>,
    pub capabilities: BTreeSet<Capability>,
    pub links: BTreeMap<String, ObjectId>,
    pub initial_grants: BTreeMap<SubjectId, BTreeSet<Capability>>,
}

impl CreateObject {
    #[must_use]
    pub fn new(type_id: TypeId, state: impl Into<Vec<u8>>) -> Self {
        Self {
            id: ObjectId::new(),
            type_id,
            parent: None,
            state: state.into(),
            capabilities: all_capabilities(),
            links: BTreeMap::new(),
            initial_grants: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn with_id(mut self, id: ObjectId) -> Self {
        self.id = id;
        self
    }

    #[must_use]
    pub fn with_parent(mut self, parent: ObjectId) -> Self {
        self.parent = Some(parent);
        self
    }

    #[must_use]
    pub fn with_link(mut self, name: impl Into<String>, target: ObjectId) -> Self {
        self.links.insert(name.into(), target);
        self
    }

    /// Adds a capability grant that becomes visible atomically with creation.
    #[must_use]
    pub fn with_grant(mut self, subject: SubjectId, capability: Capability) -> Self {
        self.initial_grants
            .entry(subject)
            .or_default()
            .insert(capability);
        self
    }
}

#[derive(Debug, Clone)]
struct AccessPolicy {
    owner: SubjectId,
    grants: BTreeMap<SubjectId, BTreeSet<Capability>>,
}

impl AccessPolicy {
    fn allows(&self, subject: SubjectId, capability: Capability) -> bool {
        subject == SYSTEM_SUBJECT
            || self.owner == subject
            || self
                .grants
                .get(&subject)
                .is_some_and(|capabilities| capabilities.contains(&capability))
    }
}

#[derive(Debug, Clone)]
struct ObjectRecord {
    header: ObjectHeader,
    retired_at_unix_ms: Option<u64>,
    state: Arc<[u8]>,
    children: BTreeSet<ObjectId>,
    links: BTreeMap<String, ObjectId>,
    capabilities: BTreeSet<Capability>,
    policy: AccessPolicy,
}

impl ObjectRecord {
    fn require(&self, context: AccessContext, capability: Capability) -> Result<(), OmsError> {
        if self.header.lifecycle == LifecycleState::Tombstoned && capability != Capability::Inspect
        {
            return Err(OmsError::InvalidLifecycle {
                object: self.header.id,
                state: self.header.lifecycle,
            });
        }
        if !self.capabilities.contains(&capability)
            || !self.policy.allows(context.subject, capability)
        {
            return Err(OmsError::Denied {
                object: self.header.id,
                capability,
            });
        }
        Ok(())
    }

    fn view(&self) -> ObjectView {
        ObjectView {
            header: self.header.clone(),
            state: Arc::clone(&self.state),
            children: Arc::new(self.children.clone()),
            links: Arc::new(self.links.clone()),
            capabilities: Arc::new(self.capabilities.clone()),
            owner: self.policy.owner,
            grants: Arc::new(self.policy.grants.clone()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ObjectView {
    header: ObjectHeader,
    state: Arc<[u8]>,
    children: Arc<BTreeSet<ObjectId>>,
    links: Arc<BTreeMap<String, ObjectId>>,
    capabilities: Arc<BTreeSet<Capability>>,
    owner: SubjectId,
    grants: Arc<BTreeMap<SubjectId, BTreeSet<Capability>>>,
}

impl ObjectView {
    #[must_use]
    pub const fn header(&self) -> &ObjectHeader {
        &self.header
    }

    #[must_use]
    pub fn state(&self) -> &[u8] {
        &self.state
    }

    #[must_use]
    pub fn children(&self) -> &BTreeSet<ObjectId> {
        &self.children
    }

    #[must_use]
    pub fn links(&self) -> &BTreeMap<String, ObjectId> {
        &self.links
    }

    #[must_use]
    pub fn capabilities(&self) -> &BTreeSet<Capability> {
        &self.capabilities
    }

    #[must_use]
    pub const fn owner(&self) -> SubjectId {
        self.owner
    }

    #[must_use]
    pub fn grants(&self) -> &BTreeMap<SubjectId, BTreeSet<Capability>> {
        &self.grants
    }
}
