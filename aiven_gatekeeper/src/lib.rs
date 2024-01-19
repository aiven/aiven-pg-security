
use std::ffi::CStr;

use pgrx::prelude::*;
use pgrx::GucRegistry;
use pgrx::GucFlags;
use pgrx::GucSetting;
use pgrx::GucContext;

use roles::is_allowed_superuser_role;
use roles::is_role_modify_allowed;
use roles::is_restricted_role_or_grant;

pgrx::pg_module_magic!();

mod roles;
use crate::roles::is_elevated;

static mut PREV_PROCESS_UTILITY_HOOK: pg_sys::ProcessUtility_hook_type = None;
static mut PREV_EXECUTOR_START_HOOK: pg_sys::ExecutorStart_hook_type = None;
static mut NEXT_OAT_HOOK: pg_sys::object_access_hook_type = None;
static GUC_IS_STRICT: GucSetting<bool> = GucSetting::<bool>::new(false);
static GUC_AGENT_IS_ENABLED: GucSetting<bool> = GucSetting::<bool>::new(true);
static GUC_RESERVED_SU_ROLES: GucSetting<Option<&'static CStr>> =
            GucSetting::<Option<&'static CStr>>::new(Some(unsafe {
                CStr::from_bytes_with_nul_unchecked(b"postgres\0")
            }));
const OAT_FUNCTION_EXECUTE:u32 = 4; // pgrx doesn't have the enum type for ObjectAccessType

// pgrx doesn't compile the extension correctly if I don't set this macro
// on atleast one function
#[pg_extern]
fn is_true() -> bool {
    return true;
}

fn is_security_restricted() -> bool {
    return unsafe {pg_sys::InSecurityRestrictedOperation()};
}

fn is_strict_mode_enabled() -> bool {
    return GUC_IS_STRICT.get();
}

fn is_agent_enabled() -> bool {
    return GUC_AGENT_IS_ENABLED.get();
}

fn copy_stmt_checks(stmt: *mut pg_sys::Node) {
    let copy_stmt: PgBox<pg_sys::CopyStmt> = unsafe {PgBox::from_pg(stmt as *mut pg_sys::CopyStmt)};
    // always deny access to code execution
    if copy_stmt.is_program {
        pg_sys::error!("COPY TO/FROM PROGRAM not allowed");
    }

    // otherwise, check if we are trying to read from file and are in a context that allows file system access
    // copy_stmt.filename pointer will be NULL for STDIN
    if copy_stmt.filename.is_null() == false {
        // strict
        if is_strict_mode_enabled() {
          pg_sys::error!("COPY TO/FROM FILE not allowed");
        }
        // creating extension
        if is_security_restricted() {
            pg_sys::error!("COPY TO/FROM FILE not allowed in extensions");
        }
        // security restricted
        if is_security_restricted(){
            pg_sys::error!("COPY TO/FROM FILE not allowed in SECURITY_RESTRICTED_OPERATION");
        }
        // elevated
        if is_elevated(){
          pg_sys::error!("COPY TO/FROM FILE not allowed");
        }
    }
}

fn create_extension_checks(stmt: *mut pg_sys::Node) {
    // get extension statement and name of extension
    let create_ext_stmt: PgBox<pg_sys::CreateExtensionStmt>;
    let extname: String;
    unsafe {
        create_ext_stmt = PgBox::from_pg(stmt as *mut pg_sys::CreateExtensionStmt);
        extname= std::ffi::CStr::from_ptr(create_ext_stmt.extname).to_string_lossy().into_owned();
    }
    // check if disallowed extension
    if extname == "file_fdw" { // error and abort the current transaction if disallowed extension
        pg_sys::error!("{} extension not allowed", extname);
    }
}

fn create_role_checks(stmt: *mut pg_sys::Node) {
    let create_role_stmt: PgBox<pg_sys::CreateRoleStmt>;
    let mut option: PgBox<pg_sys::DefElem>;
    unsafe {
        create_role_stmt = PgBox::from_pg(stmt as *mut pg_sys::CreateRoleStmt);
        let options_lst = pgrx::PgList::from_pg(create_role_stmt.options);
        for opt_raw in options_lst.iter_ptr() {
            option = PgBox::from_pg(opt_raw as *mut pg_sys::DefElem);
            let option_name = std::ffi::CStr::from_ptr(option.defname).to_string_lossy().into_owned();

            // check if role is allowed to be a superuser, is in GUC_RESERVED_SU_ROLES
            let role_name: String = std::ffi::CStr::from_ptr(create_role_stmt.role).to_string_lossy().into_owned();
            if is_allowed_superuser_role(role_name.clone(), GUC_RESERVED_SU_ROLES.get().unwrap().to_str().unwrap()) == false {
                pg_sys::error!("Role {} not in permitted superuser list", role_name)
            }
            // check if we are setting superuser true
            if option_name == "superuser" && pg_sys::defGetBoolean(option.as_ptr()) {
                let (allowed, msg) = is_role_modify_allowed(is_strict_mode_enabled());
                if allowed == false {
                    pg_sys::error!("{}", msg)
                }
            }
        }
    }

}

fn alter_role_checks(stmt: *mut pg_sys::Node) {
    let alter_role_stmt: PgBox<pg_sys::AlterRoleStmt>;
    let mut option: PgBox<pg_sys::DefElem>;
    unsafe {
        alter_role_stmt = PgBox::from_pg(stmt as *mut pg_sys::AlterRoleStmt);
        // check we aren't altering a reserved role (existing superuser)
        let role_oid = pg_sys::get_rolespec_oid(alter_role_stmt.role, true);
        if is_restricted_role_or_grant(role_oid){
            let (allowed, msg) = is_role_modify_allowed(is_strict_mode_enabled());
                if allowed == false {
                    pg_sys::error!("{}", msg)
                }
        }

        let options_lst = pgrx::PgList::from_pg(alter_role_stmt.options);
        for opt_raw in options_lst.iter_ptr() {
            option = PgBox::from_pg(opt_raw as *mut pg_sys::DefElem);
            let option_name = std::ffi::CStr::from_ptr(option.defname).to_string_lossy().into_owned();

            // check if role is allowed to be a superuser, is in GUC_RESERVED_SU_ROLES
            let role_name: String = std::ffi::CStr::from_ptr((*alter_role_stmt.role).rolename).to_string_lossy().into_owned();
            if is_allowed_superuser_role(role_name.clone(), GUC_RESERVED_SU_ROLES.get().unwrap().to_str().unwrap()) == false {
                pg_sys::error!("Role {} not in permitted superuser list", role_name)
            }
            // check if we are setting superuser true
            if option_name == "superuser" && pg_sys::defGetBoolean(option.as_ptr()) {
                let (allowed, msg) = is_role_modify_allowed(is_strict_mode_enabled());
                if allowed == false {
                    pg_sys::error!("{}", msg)
                }
            }
        }
    }

}

fn grant_role_checks(stmt: *mut pg_sys::Node) {
    let grant_role_stmt: PgBox<pg_sys::GrantRoleStmt>;
    let mut access_privilege: PgBox<pg_sys::AccessPriv>;

    unsafe {
        grant_role_stmt = PgBox::from_pg(stmt as *mut pg_sys::GrantRoleStmt);
        let lst = pgrx::PgList::from_pg(grant_role_stmt.granted_roles);
        for granted_role in lst.iter_ptr() {
            access_privilege = PgBox::from_pg(granted_role as *mut pg_sys::AccessPriv);
            //let priv_name = std::ffi::CStr::from_ptr(access_privilege.priv_name).to_string_lossy().into_owned();
            let roloid : pg_sys::Oid = pg_sys::get_role_oid(access_privilege.priv_name, false);
            if is_restricted_role_or_grant(roloid) {
                let (allowed, msg) = is_role_modify_allowed(is_strict_mode_enabled());
                if allowed == false {
                    pg_sys::error!("{}", msg)
                }
            }
        }
    }
}

fn function_create_checks(stmt: *mut pg_sys::Node) {

}

#[pg_guard]
extern "C" fn executor_start_hook(query_desc: *mut pg_sys::QueryDesc, eflags: i32) {
    info!("ExecutorStart");
    unsafe {
        if let Some(prev_hook) = PREV_EXECUTOR_START_HOOK {
            prev_hook(query_desc, eflags);
        } else {
            pg_sys::standard_ExecutorStart(query_desc, eflags);
        }
    }
}

#[allow(clippy::too_many_arguments)]
#[pg_guard]
extern "C" fn process_utility_hook(
  pstmt: *mut pg_sys::PlannedStmt,
  query_string: *const std::os::raw::c_char,
  read_only_tree: bool,
  context: pg_sys::ProcessUtilityContext,
  params: pg_sys::ParamListInfo,
  query_env: *mut pg_sys::QueryEnvironment,
  dest: *mut pg_sys::DestReceiver,
  qc: *mut pg_sys::QueryCompletion,
) {

    // only run checks if the agent is actually enabled, just skip all of this
    if is_agent_enabled() {
        let stmt: *mut pg_sys::Node = unsafe {(*pstmt).utilityStmt };
        let stmt_type: pg_sys::NodeTag = unsafe { (*stmt).type_ };

        match stmt_type{
            pg_sys::NodeTag::T_AlterRoleStmt=>alter_role_checks(stmt),
            pg_sys::NodeTag::T_CreateRoleStmt=>create_role_checks(stmt),
            pg_sys::NodeTag::T_DropRoleStmt=>(), // should check that trusted roles aren't dropped
            pg_sys::NodeTag::T_GrantRoleStmt=>grant_role_checks(stmt),
            pg_sys::NodeTag::T_CopyStmt=>copy_stmt_checks(stmt),
            pg_sys::NodeTag::T_VariableSetStmt=>(), // currently we don't do any checks on VariableSet
            pg_sys::NodeTag::T_CreateFunctionStmt=>function_create_checks(stmt),
            pg_sys::NodeTag::T_CreateExtensionStmt=>create_extension_checks(stmt),
            _=> (),
        }
    }

    unsafe {
        if let Some(prev_hook) = PREV_PROCESS_UTILITY_HOOK {
            prev_hook(
            pstmt,
            query_string,
            read_only_tree,
            context,
            params,
            query_env,
            dest,
            qc,
            );
        } else {
            pg_sys::standard_ProcessUtility(
            pstmt,
            query_string,
            read_only_tree,
            context,
            params,
            query_env,
            dest,
            qc,
            );
        }
    }
}

#[pg_guard]
extern "C" fn object_access_hook(
        access: pg_sys::ObjectAccessType,
        class_id: pg_sys::Oid,
        object_id: pg_sys::Oid,
        sub_id: ::std::os::raw::c_int,
        arg: *mut ::std::os::raw::c_void
) {
    // only if the agent is enabled and this prior to the execution of a function
    if is_agent_enabled() && access == OAT_FUNCTION_EXECUTE {
        // check object access restrictions

    }

    // continue
    unsafe {
        if let Some(prev_hook) = NEXT_OAT_HOOK {
            prev_hook(access, class_id, object_id, sub_id, arg,);
        }
        // there is no else here, unlike ProcessUtility and ExecutorStart, this is the same
        // as the C version of gatekeeper and is because there is no standard_ObjectAccess function
        // like there is for standard_ProcessUtility etc
    }
}

#[pg_guard]
pub extern "C" fn _PG_init() {
    unsafe {
        // must be loaded as a shared_library
        if !pg_sys::process_shared_preload_libraries_in_progress {
            error!("aiven_pg_gatekeeper is not in shared_preload_libraries");
        }

        GucRegistry::define_bool_guc(
            "aiven.pg_security_agent",
            "Toggle the security agent checks on and off",
            "Toggle the security agent checks on and off",
            &GUC_AGENT_IS_ENABLED,
            GucContext::Sighup,
            GucFlags::SUPERUSER_ONLY|GucFlags::DISALLOW_IN_AUTO_FILE|GucFlags::NOT_WHILE_SEC_REST,
        );

        GucRegistry::define_bool_guc(
            "aiven.pg_security_agent_strict",
            "Toggle the agent into strict mode. Reserved actions are blocked regardless of context",
            "Toggle the agent into strict mode. Reserved actions are blocked regardless of context",
            &GUC_IS_STRICT,
            GucContext::Postmaster,
            GucFlags::SUPERUSER_ONLY|GucFlags::DISALLOW_IN_AUTO_FILE|GucFlags::NOT_WHILE_SEC_REST,
        );

        GucRegistry::define_string_guc(
            "aiven.pg_security_agent_reserved_roles",
            "Comma-separated list of roles that can be assigned superuser",
            "Comma-separated list of roles that can be assigned superuser",
            &GUC_RESERVED_SU_ROLES,
            GucContext::Postmaster,
            GucFlags::SUPERUSER_ONLY|GucFlags::DISALLOW_IN_AUTO_FILE|GucFlags::NOT_WHILE_SEC_REST|GucFlags::NO_SHOW_ALL,
        );

        PREV_EXECUTOR_START_HOOK = pg_sys::ExecutorStart_hook;
        pg_sys::ExecutorStart_hook = Some(executor_start_hook);

        PREV_PROCESS_UTILITY_HOOK = pg_sys::ProcessUtility_hook;
        pg_sys::ProcessUtility_hook = Some(process_utility_hook);

        NEXT_OAT_HOOK = pg_sys::object_access_hook;
        pg_sys::object_access_hook = Some(object_access_hook)
    }
}

#[pg_guard]
pub extern "C" fn _PG_fini() {
    unsafe {
        pg_sys::ExecutorStart_hook = PREV_EXECUTOR_START_HOOK;
        pg_sys::ProcessUtility_hook = PREV_PROCESS_UTILITY_HOOK;
        pg_sys::object_access_hook = NEXT_OAT_HOOK;
    }
}

#[cfg(any(test, feature = "pg_test"))]
#[pg_schema]
mod tests {
    use pgrx::prelude::*;

    #[pg_test]
    fn test_hello_aiven_gatekeeper() {
    }

}

/// This module is required by `cargo pgrx test` invocations.
/// It must be visible at the root of your extension crate.
#[cfg(test)]
pub mod pg_test {
    pub fn setup(_options: Vec<&str>) {
        // perform one-off initialization when the pg_test framework starts
    }

    pub fn postgresql_conf_options() -> Vec<&'static str> {
        // return any postgresql.conf settings that are required for your tests
        vec![]
    }
}