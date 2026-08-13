//! Maps unpacked 1C configuration modules to the identifiers used by RDBG.

use anyhow::{Context, Result, bail};
use quick_xml::Reader;
use quick_xml::events::Event;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

const COMMON_MODULE_PROPERTY_ID: &str = "d5963243-262e-4398-b4d7-fb16d06484f6";
const MODULE_PROPERTY_ID: &str = "32e087ab-1491-49b6-aba7-43571b41ac2b";
const COMMAND_MODULE_PROPERTY_ID: &str = "078a6af8-d22c-4248-9c33-7e90075a3d2c";
const OBJECT_MODULE_PROPERTY_ID: &str = "a637f77f-3840-441d-a1c3-699c8c5cb7e0";
const MANAGER_MODULE_PROPERTY_ID: &str = "d1b64a2c-8078-4982-8190-8f81aefda192";
const RECORD_SET_MODULE_PROPERTY_ID: &str = "9f36fd70-4bf4-47f6-b235-935f73aab43f";
const VALUE_MANAGER_MODULE_PROPERTY_ID: &str = "3e58c91f-9aaa-4f42-8999-4baf33907b75";
const MANAGED_APPLICATION_MODULE_PROPERTY_ID: &str = "d22e852a-cf8a-4f77-8ccb-3548e7792bea";
const SESSION_MODULE_PROPERTY_ID: &str = "9b7bbbae-9771-46f2-9e4d-2489e0ffc702";
const EXTERNAL_CONNECTION_MODULE_PROPERTY_ID: &str = "a4a9c1e2-1e54-4c7f-af06-4ca341198fac";
const ORDINARY_APPLICATION_MODULE_PROPERTY_ID: &str = "a78d9ce3-4e0c-48d5-9863-ae7342eedf94";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleInfo {
    pub extension_name: String,
    pub object_id: String,
    pub property_id: String,
}

#[derive(Debug, Default)]
pub struct ModuleRegistry {
    by_path: HashMap<PathBuf, ModuleInfo>,
    by_identity: HashMap<(String, String, String), PathBuf>,
}

impl ModuleRegistry {
    pub fn load(root_project: &Path, extensions: &[PathBuf]) -> Result<Self> {
        let root_project = root_project.canonicalize().with_context(|| {
            format!(
                "cannot resolve base configuration source directory {}",
                root_project.display()
            )
        })?;
        let mut registry = Self::default();
        registry.scan_root(&root_project, "")?;

        let mut extension_paths = HashMap::<PathBuf, PathBuf>::new();
        let mut extension_names = HashMap::<String, PathBuf>::new();
        for extension_path in extensions {
            let canonical_path = extension_path.canonicalize().with_context(|| {
                format!(
                    "cannot resolve extension configuration source directory {}",
                    extension_path.display()
                )
            })?;
            let extension_name = configuration_name(&canonical_path)?;

            if canonical_path == root_project {
                bail!(
                    "extension configuration source {} is the same as base configuration source {}",
                    extension_path.display(),
                    root_project.display()
                );
            }
            if let Some(previous_path) = extension_paths.get(&canonical_path) {
                bail!(
                    "extension configuration {} ({}) duplicates extension configuration {} ({})",
                    extension_path.display(),
                    extension_name,
                    previous_path.display(),
                    extension_name
                );
            }
            if !extension_name.trim().is_empty()
                && let Some(previous_path) = extension_names.get(&extension_name)
            {
                bail!(
                    "extension configurations {} and {} have the same logical name {:?}",
                    previous_path.display(),
                    canonical_path.display(),
                    extension_name
                );
            }

            extension_paths.insert(canonical_path.clone(), extension_path.clone());
            if !extension_name.trim().is_empty() {
                extension_names.insert(extension_name.clone(), canonical_path.clone());
            }
            registry.scan_root(&canonical_path, &extension_name)?;
        }
        Ok(registry)
    }

    pub fn module_by_path(&self, path: &Path) -> Result<&ModuleInfo> {
        let canonical_path = path
            .canonicalize()
            .with_context(|| format!("cannot resolve source path {}", path.display()))?;
        self.by_path.get(&canonical_path).with_context(|| {
            format!(
                "the BSL file {} is not a known configuration module",
                path.display()
            )
        })
    }

    pub fn path_by_module(
        &self,
        extension_name: &str,
        object_id: &str,
        property_id: &str,
    ) -> Option<&Path> {
        self.by_identity
            .get(&(
                extension_name.to_owned(),
                object_id.to_owned(),
                property_id.to_owned(),
            ))
            .map(PathBuf::as_path)
    }

    fn scan_root(&mut self, root: &Path, extension_name: &str) -> Result<()> {
        if root.join("DT-INF/PROJECT.PMF").is_file() {
            return self.scan_edt_root(root, extension_name);
        }
        let configuration_xml = root.join("Configuration.xml");
        let configuration_id = metadata_uuid(&configuration_xml)?;
        let ext_path = root.join("Ext");
        if ext_path.is_dir() {
            for module_path in bsl_files(&ext_path)? {
                self.cache_module(
                    &module_path,
                    extension_name,
                    &configuration_id,
                    property_id("", module_stem(&module_path)?)?,
                )?;
            }
        }

        for directory in fs::read_dir(root)
            .with_context(|| format!("cannot read configuration directory {}", root.display()))?
        {
            let directory = directory?;
            if !directory.file_type()?.is_dir() || directory.file_name() == "Ext" {
                continue;
            }
            for entry in fs::read_dir(directory.path())? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().is_some_and(|extension| extension == "xml") {
                    self.scan_metadata_xml(&path, extension_name)?;
                }
            }
        }
        Ok(())
    }

    fn scan_edt_root(&mut self, root: &Path, extension_name: &str) -> Result<()> {
        let source_root = root.join("src");
        let configuration = source_root.join("Configuration/Configuration.mdo");
        let configuration_id = edt_uuid(&configuration)?;

        for module_path in bsl_files(&source_root.join("Configuration"))? {
            self.cache_module(
                &module_path,
                extension_name,
                &configuration_id,
                property_id("", module_stem(&module_path)?)?,
            )?;
        }

        for metadata_path in edt_metadata_files(&source_root)? {
            let object_id = edt_uuid(&metadata_path)?;
            let metadata_type = edt_metadata_type(&source_root, &metadata_path)?;
            let object_dir = metadata_path
                .parent()
                .context("EDT metadata file has no parent directory")?;

            for module_path in direct_bsl_files(object_dir)? {
                self.cache_module(
                    &module_path,
                    extension_name,
                    &object_id,
                    property_id(metadata_type, module_stem(&module_path)?)?,
                )?;
            }

            let forms_dir = object_dir.join("Forms");
            if !forms_dir.is_dir() {
                continue;
            }
            for form_dir in fs::read_dir(forms_dir)? {
                let form_dir = form_dir?;
                if !form_dir.file_type()?.is_dir() {
                    continue;
                }
                let form_name = form_dir.file_name();
                let form_id = edt_form_uuid(&metadata_path, &form_name)?;
                let module_path = form_dir.path().join("Module.bsl");
                if module_path.is_file() {
                    self.cache_module(&module_path, extension_name, &form_id, MODULE_PROPERTY_ID)?;
                }
            }
        }
        Ok(())
    }

    fn scan_metadata_xml(&mut self, metadata_xml: &Path, extension_name: &str) -> Result<()> {
        let object_id = metadata_uuid(metadata_xml)?;
        let metadata_name = metadata_xml
            .file_stem()
            .context("metadata file has no name")?;
        let metadata_path = metadata_xml.with_file_name(metadata_name);
        let metadata_type = metadata_path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .context("cannot determine metadata type")?;

        let ext_path = metadata_path.join("Ext");
        if ext_path.is_dir() {
            for module_path in bsl_files(&ext_path)? {
                self.cache_module(
                    &module_path,
                    extension_name,
                    &object_id,
                    property_id(metadata_type, module_stem(&module_path)?)?,
                )?;
            }
        }

        let forms_path = metadata_path.join("Forms");
        if forms_path.is_dir() {
            for form_xml in fs::read_dir(&forms_path)? {
                let form_xml = form_xml?.path();
                if form_xml
                    .extension()
                    .is_none_or(|extension| extension != "xml")
                {
                    continue;
                }
                let form_id = metadata_uuid(&form_xml)?;
                let form_path = form_xml.with_extension("");
                if form_path.is_dir() {
                    for module_path in bsl_files(&form_path)? {
                        self.cache_module(
                            &module_path,
                            extension_name,
                            &form_id,
                            property_id(metadata_type, module_stem(&module_path)?)?,
                        )?;
                    }
                }
            }
        }
        Ok(())
    }

    fn cache_module(
        &mut self,
        path: &Path,
        extension_name: &str,
        object_id: &str,
        property_id: &str,
    ) -> Result<()> {
        let canonical_path = path
            .canonicalize()
            .with_context(|| format!("cannot resolve module path {}", path.display()))?;
        let info = ModuleInfo {
            extension_name: extension_name.to_owned(),
            object_id: object_id.to_owned(),
            property_id: property_id.to_owned(),
        };
        let identity = (
            info.extension_name.clone(),
            info.object_id.clone(),
            info.property_id.clone(),
        );
        if let Some(previous_path) = self.by_identity.get(&identity) {
            bail!(
                "module identity collision for extension {:?}, object {}, property {}: {} and {}",
                info.extension_name,
                info.object_id,
                info.property_id,
                previous_path.display(),
                canonical_path.display()
            );
        }
        self.by_identity.insert(identity, canonical_path.clone());
        self.by_path.insert(canonical_path, info);
        Ok(())
    }
}

fn bsl_files(directory: &Path) -> Result<Vec<PathBuf>> {
    if !directory.is_dir() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            files.extend(bsl_files(&path)?);
        } else if path.extension().is_some_and(|extension| extension == "bsl") {
            files.push(path);
        }
    }
    Ok(files)
}

fn direct_bsl_files(directory: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_file()
            && path.extension().is_some_and(|extension| extension == "bsl")
        {
            files.push(path);
        }
    }
    Ok(files)
}

fn edt_metadata_files(source_root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_edt_metadata_files(source_root, &mut files)?;
    Ok(files)
}

fn collect_edt_metadata_files(directory: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            // Global configuration modules are handled separately, avoiding
            // duplicate identity registrations for Configuration.mdo.
            if entry.file_name() != "Configuration" {
                collect_edt_metadata_files(&path, files)?;
            }
        } else if path.extension().is_some_and(|extension| extension == "mdo") {
            files.push(path);
        }
    }
    Ok(())
}

fn edt_metadata_type<'a>(source_root: &Path, metadata_path: &'a Path) -> Result<&'a str> {
    metadata_path
        .strip_prefix(source_root)
        .context("EDT metadata is outside source root")?
        .components()
        .next()
        .and_then(|component| component.as_os_str().to_str())
        .context("cannot determine EDT metadata type")
}

fn module_stem(path: &Path) -> Result<&str> {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .context("module file has no UTF-8 stem")
}

fn property_id(metadata_type: &str, module_name: &str) -> Result<&'static str> {
    match metadata_type {
        "CommonModules" | "WebServices" | "HTTPServices" => Ok(COMMON_MODULE_PROPERTY_ID),
        _ => match module_name {
            "Module" => Ok(MODULE_PROPERTY_ID),
            "CommandModule" => Ok(COMMAND_MODULE_PROPERTY_ID),
            "ObjectModule" => Ok(OBJECT_MODULE_PROPERTY_ID),
            "ManagerModule" => Ok(MANAGER_MODULE_PROPERTY_ID),
            "RecordSetModule" => Ok(RECORD_SET_MODULE_PROPERTY_ID),
            "ValueManagerModule" => Ok(VALUE_MANAGER_MODULE_PROPERTY_ID),
            "ManagedApplicationModule" => Ok(MANAGED_APPLICATION_MODULE_PROPERTY_ID),
            "SessionModule" => Ok(SESSION_MODULE_PROPERTY_ID),
            "ExternalConnectionModule" => Ok(EXTERNAL_CONNECTION_MODULE_PROPERTY_ID),
            "OrdinaryApplicationModule" => Ok(ORDINARY_APPLICATION_MODULE_PROPERTY_ID),
            other => bail!("unknown 1C module type {metadata_type}\\{other}"),
        },
    }
}

fn metadata_uuid(path: &Path) -> Result<String> {
    let xml = fs::read_to_string(path)
        .with_context(|| format!("cannot read metadata XML {}", path.display()))?;
    let mut reader = Reader::from_str(&xml);
    let mut inside_metadata = false;

    loop {
        match reader.read_event()? {
            Event::Start(element) if element.local_name().as_ref() == b"MetaDataObject" => {
                inside_metadata = true;
            }
            Event::Start(element) if inside_metadata => {
                if let Some(uuid) = uuid_attribute(&element)? {
                    return Ok(uuid);
                }
            }
            Event::Empty(element) if inside_metadata => {
                if let Some(uuid) = uuid_attribute(&element)? {
                    return Ok(uuid);
                }
            }
            Event::Eof => bail!("metadata XML {} has no object UUID", path.display()),
            _ => {}
        }
    }
}

fn edt_uuid(path: &Path) -> Result<String> {
    let xml = fs::read_to_string(path)
        .with_context(|| format!("cannot read EDT metadata file {}", path.display()))?;
    let mut reader = Reader::from_str(&xml);
    loop {
        match reader.read_event()? {
            Event::Start(element) | Event::Empty(element) => {
                if let Some(uuid) = uuid_attribute(&element)? {
                    return Ok(uuid);
                }
            }
            Event::Eof => bail!("EDT metadata file {} has no object UUID", path.display()),
            _ => {}
        }
    }
}

fn edt_form_uuid(metadata_path: &Path, name: &std::ffi::OsStr) -> Result<String> {
    let form_name = name
        .to_str()
        .context("EDT form directory name is not UTF-8")?;
    let xml = fs::read_to_string(metadata_path)
        .with_context(|| format!("cannot read EDT metadata file {}", metadata_path.display()))?;
    let mut reader = Reader::from_str(&xml);
    let mut candidate_uuid = None;
    let mut is_form = false;
    loop {
        match reader.read_event()? {
            Event::Start(element) if element.local_name().as_ref() == b"forms" => {
                candidate_uuid = uuid_attribute(&element)?;
                is_form = true;
            }
            Event::End(element) if element.local_name().as_ref() == b"forms" => {
                candidate_uuid = None;
                is_form = false;
            }
            Event::Start(element) if is_form && element.local_name().as_ref() == b"name" => {
                if reader.read_text(element.name())?.as_ref() == form_name {
                    return candidate_uuid.context("EDT form has no UUID");
                }
            }
            Event::Eof => bail!(
                "EDT metadata file {} has no form named {}",
                metadata_path.display(),
                form_name
            ),
            _ => {}
        }
    }
}

fn uuid_attribute(element: &quick_xml::events::BytesStart<'_>) -> Result<Option<String>> {
    for attribute in element.attributes() {
        let attribute = attribute?;
        if attribute.key.local_name().as_ref() == b"uuid" {
            return attribute
                .unescape_value()
                .map(|value| Some(value.into_owned()))
                .context("cannot decode metadata UUID");
        }
    }
    Ok(None)
}

fn configuration_name(root: &Path) -> Result<String> {
    if root.join("DT-INF/PROJECT.PMF").is_file() {
        return edt_configuration_name(root);
    }
    let xml = fs::read_to_string(root.join("Configuration.xml"))?;
    let mut reader = Reader::from_str(&xml);
    let mut inside_properties = false;
    loop {
        match reader.read_event()? {
            Event::Start(element) if element.local_name().as_ref() == b"Properties" => {
                inside_properties = true;
            }
            Event::Start(element)
                if inside_properties && element.local_name().as_ref() == b"Name" =>
            {
                return reader
                    .read_text(element.name())
                    .map(|value| value.into_owned())
                    .context("cannot read extension configuration name");
            }
            Event::Eof => bail!("extension configuration has no name"),
            _ => {}
        }
    }
}

fn edt_configuration_name(root: &Path) -> Result<String> {
    let xml = fs::read_to_string(root.join("src/Configuration/Configuration.mdo"))?;
    let mut reader = Reader::from_str(&xml);
    loop {
        match reader.read_event()? {
            Event::Start(element) if element.local_name().as_ref() == b"name" => {
                return reader
                    .read_text(element.name())
                    .map(|value| value.into_owned())
                    .context("cannot read EDT configuration name");
            }
            Event::Eof => bail!("EDT configuration has no name"),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn temporary_root() -> PathBuf {
        std::env::temp_dir().join(format!("onec-debug-adapter-{}", Uuid::new_v4()))
    }

    fn write_configuration(root: &Path, name: &str, uuid: &str) {
        write(
            &root.join("Configuration.xml"),
            &format!(
                "<MetaDataObject><Configuration uuid=\"{uuid}\"><Properties><Name>{name}</Name></Properties></Configuration></MetaDataObject>"
            ),
        );
    }

    fn write_edt_configuration(root: &Path, name: &str, uuid: &str) {
        write(
            &root.join("DT-INF/PROJECT.PMF"),
            "Manifest-Version: 1.0\nRuntime-Version: 8.3.27\n",
        );
        write(
            &root.join("src/Configuration/Configuration.mdo"),
            &format!(
                "<mdclass:Configuration uuid=\"{uuid}\"><name>{name}</name></mdclass:Configuration>"
            ),
        );
    }

    #[test]
    fn maps_root_and_common_modules() {
        let root = temporary_root();
        write_configuration(&root, "Demo", "config-uuid");
        write(&root.join("Ext/ManagedApplicationModule.bsl"), "");
        write(
            &root.join("CommonModules/Tools.xml"),
            "<MetaDataObject><CommonModule uuid=\"tools-uuid\" /></MetaDataObject>",
        );
        write(&root.join("CommonModules/Tools/Ext/Module.bsl"), "");

        let registry = ModuleRegistry::load(&root, &[]).unwrap();
        let root_module = registry
            .module_by_path(&root.join("Ext/ManagedApplicationModule.bsl"))
            .unwrap();
        assert_eq!(root_module.object_id, "config-uuid");
        assert_eq!(
            root_module.property_id,
            MANAGED_APPLICATION_MODULE_PROPERTY_ID
        );

        let common_module = registry
            .module_by_path(&root.join("CommonModules/Tools/Ext/Module.bsl"))
            .unwrap();
        assert_eq!(common_module.object_id, "tools-uuid");
        assert_eq!(common_module.property_id, COMMON_MODULE_PROPERTY_ID);
        assert_eq!(
            registry
                .path_by_module("", "tools-uuid", COMMON_MODULE_PROPERTY_ID)
                .unwrap(),
            root.join("CommonModules/Tools/Ext/Module.bsl")
                .canonicalize()
                .unwrap()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn maps_edt_configuration_object_and_form_modules() {
        let root = temporary_root();
        write_edt_configuration(&root, "Demo", "configuration-uuid");
        write(
            &root.join("src/Configuration/ManagedApplicationModule.bsl"),
            "",
        );
        write(
            &root.join("src/CommonModules/Tools/Tools.mdo"),
            "<mdclass:CommonModule uuid=\"common-uuid\"><name>Tools</name></mdclass:CommonModule>",
        );
        write(&root.join("src/CommonModules/Tools/Module.bsl"), "");
        write(
            &root.join("src/Catalogs/Items/Items.mdo"),
            "<mdclass:Catalog uuid=\"catalog-uuid\"><name>Items</name><forms uuid=\"form-uuid\"><name>List</name></forms></mdclass:Catalog>",
        );
        write(&root.join("src/Catalogs/Items/ObjectModule.bsl"), "");
        write(
            &root.join("src/Catalogs/Items/Commands/Refresh/Refresh.mdo"),
            "<mdclass:Command uuid=\"command-uuid\"><name>Refresh</name></mdclass:Command>",
        );
        write(
            &root.join("src/Catalogs/Items/Commands/Refresh/CommandModule.bsl"),
            "",
        );
        write(
            &root.join("src/Catalogs/Items/Forms/List/Form.form"),
            "<form:Form/>",
        );
        write(&root.join("src/Catalogs/Items/Forms/List/Module.bsl"), "");

        let registry = ModuleRegistry::load(&root, &[]).unwrap();
        assert_eq!(
            registry
                .module_by_path(&root.join("src/Configuration/ManagedApplicationModule.bsl"))
                .unwrap()
                .object_id,
            "configuration-uuid"
        );
        let common = registry
            .module_by_path(&root.join("src/CommonModules/Tools/Module.bsl"))
            .unwrap();
        assert_eq!(common.object_id, "common-uuid");
        assert_eq!(common.property_id, COMMON_MODULE_PROPERTY_ID);
        let object = registry
            .module_by_path(&root.join("src/Catalogs/Items/ObjectModule.bsl"))
            .unwrap();
        assert_eq!(object.object_id, "catalog-uuid");
        assert_eq!(object.property_id, OBJECT_MODULE_PROPERTY_ID);
        let command = registry
            .module_by_path(&root.join("src/Catalogs/Items/Commands/Refresh/CommandModule.bsl"))
            .unwrap();
        assert_eq!(command.object_id, "command-uuid");
        assert_eq!(command.property_id, COMMAND_MODULE_PROPERTY_ID);
        let form = registry
            .module_by_path(&root.join("src/Catalogs/Items/Forms/List/Module.bsl"))
            .unwrap();
        assert_eq!(form.object_id, "form-uuid");
        assert_eq!(form.property_id, MODULE_PROPERTY_ID);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_extension_equal_to_base_configuration() {
        let root = temporary_root();
        write_configuration(&root, "Demo", "config-uuid");

        let error = ModuleRegistry::load(&root, std::slice::from_ref(&root))
            .unwrap_err()
            .to_string();

        assert!(error.contains("same as base configuration source"));
        assert!(error.contains(&root.display().to_string()));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_duplicate_extension_canonical_path() {
        let root = temporary_root();
        let extension = root.join("Extension");
        write_configuration(&root, "Demo", "config-uuid");
        write_configuration(&extension, "Extension", "extension-uuid");
        let duplicate_spelling = extension.join(".");

        let error = ModuleRegistry::load(&root, &[extension.clone(), duplicate_spelling.clone()])
            .unwrap_err()
            .to_string();

        assert!(error.contains("duplicates extension configuration"));
        assert!(error.contains(&extension.display().to_string()));
        assert!(error.contains(&duplicate_spelling.display().to_string()));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_extensions_with_duplicate_logical_name() {
        let root = temporary_root();
        let first_extension = root.join("FirstExtension");
        let second_extension = root.join("SecondExtension");
        write_configuration(&root, "Demo", "config-uuid");
        write_configuration(&first_extension, "SharedExtension", "extension-one-uuid");
        write_configuration(&second_extension, "SharedExtension", "extension-two-uuid");

        let error =
            ModuleRegistry::load(&root, &[first_extension.clone(), second_extension.clone()])
                .unwrap_err()
                .to_string();

        assert!(error.contains("same logical name \"SharedExtension\""));
        assert!(error.contains(&first_extension.display().to_string()));
        assert!(error.contains(&second_extension.display().to_string()));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_module_identity_collision() {
        let root = temporary_root();
        write_configuration(&root, "Demo", "config-uuid");
        write(
            &root.join("CommonModules/First.xml"),
            "<MetaDataObject><CommonModule uuid=\"shared-uuid\" /></MetaDataObject>",
        );
        write(&root.join("CommonModules/First/Ext/Module.bsl"), "");
        write(
            &root.join("CommonModules/Second.xml"),
            "<MetaDataObject><CommonModule uuid=\"shared-uuid\" /></MetaDataObject>",
        );
        write(&root.join("CommonModules/Second/Ext/Module.bsl"), "");

        let error = ModuleRegistry::load(&root, &[]).unwrap_err().to_string();

        assert!(error.contains("module identity collision"));
        assert!(error.contains("extension \"\""));
        assert!(error.contains("First/Ext/Module.bsl"));
        assert!(error.contains("Second/Ext/Module.bsl"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn supports_multiple_distinct_extensions() {
        let root = temporary_root();
        let first_extension = root.join("FirstExtension");
        let second_extension = root.join("SecondExtension");
        write_configuration(&root, "Demo", "config-uuid");
        write_configuration(&first_extension, "First", "extension-one-uuid");
        write_configuration(&second_extension, "Second", "extension-two-uuid");
        write(
            &first_extension.join("Ext/ManagedApplicationModule.bsl"),
            "",
        );
        write(
            &second_extension.join("Ext/ManagedApplicationModule.bsl"),
            "",
        );

        let registry =
            ModuleRegistry::load(&root, &[first_extension.clone(), second_extension.clone()])
                .unwrap();

        assert_eq!(
            registry
                .path_by_module(
                    "First",
                    "extension-one-uuid",
                    MANAGED_APPLICATION_MODULE_PROPERTY_ID
                )
                .unwrap(),
            first_extension
                .join("Ext/ManagedApplicationModule.bsl")
                .canonicalize()
                .unwrap()
        );
        assert_eq!(
            registry
                .path_by_module(
                    "Second",
                    "extension-two-uuid",
                    MANAGED_APPLICATION_MODULE_PROPERTY_ID
                )
                .unwrap(),
            second_extension
                .join("Ext/ManagedApplicationModule.bsl")
                .canonicalize()
                .unwrap()
        );
        fs::remove_dir_all(root).unwrap();
    }
}
