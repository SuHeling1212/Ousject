use super::{
    Arc, AuthService, DEFAULT_STEP_LIMIT, InMemoryObjectManager, Path, PathBuf, SYSTEM_SUBJECT,
    SubjectId, error_text,
};

#[derive(Debug)]
pub(crate) struct RuntimeOptions {
    pub(crate) state: Option<PathBuf>,
    pub(crate) steps: u64,
    pub(crate) capability: Option<String>,
    pub(crate) session: Option<String>,
    pub(crate) local: bool,
}

pub(crate) fn parse_options(arguments: &[String]) -> Result<(Vec<String>, RuntimeOptions), String> {
    let mut positional = Vec::new();
    let mut state = Some(PathBuf::from(".ousject/objects.oms"));
    let mut steps = DEFAULT_STEP_LIMIT;
    let mut capability = None;
    let mut session = None;
    let mut local = false;
    let mut position = 0;
    while position < arguments.len() {
        match arguments[position].as_str() {
            "--state" => {
                position += 1;
                let value = arguments
                    .get(position)
                    .ok_or_else(|| "--state requires a path".to_owned())?;
                state = Some(PathBuf::from(value));
            }
            "--memory" => state = None,
            "--steps" => {
                position += 1;
                let value = arguments
                    .get(position)
                    .ok_or_else(|| "--steps requires a number".to_owned())?;
                steps = value
                    .parse::<u64>()
                    .map_err(|_| "--steps must be an unsigned integer".to_owned())?;
            }
            "--capability" => {
                position += 1;
                capability = Some(
                    arguments
                        .get(position)
                        .ok_or_else(|| "--capability requires a name".to_owned())?
                        .to_owned(),
                );
            }
            "--session" => {
                position += 1;
                session = Some(
                    arguments
                        .get(position)
                        .ok_or_else(|| "--session requires a token".to_owned())?
                        .to_owned(),
                );
            }
            "--local" => local = true,
            value if value.starts_with('-') => return Err(format!("unknown option '{value}'")),
            value => positional.push(value.to_owned()),
        }
        position += 1;
    }
    Ok((
        positional,
        RuntimeOptions {
            state,
            steps,
            capability,
            session,
            local,
        },
    ))
}

pub(crate) fn option_subject(
    manager: &Arc<InMemoryObjectManager>,
    options: &RuntimeOptions,
) -> Result<SubjectId, String> {
    match (&options.session, options.local) {
        (Some(_), true) => Err("use either --session or --local, not both".to_owned()),
        (Some(token), false) => AuthService::new(Arc::clone(manager))
            .authenticate(token)
            .map_err(error_text),
        (None, true) => Ok(SYSTEM_SUBJECT),
        (None, false) => Err(
            "authentication required: use --session <token>; --local is only for explicit development/recovery work"
                .to_owned(),
        ),
    }
}

pub(crate) fn open_manager(path: Option<&Path>) -> Result<Arc<InMemoryObjectManager>, String> {
    let manager = match path {
        Some(path) => InMemoryObjectManager::open_persistent(path),
        None => InMemoryObjectManager::new(1),
    }
    .map_err(error_text)?;
    Ok(Arc::new(manager))
}
