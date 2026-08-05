use uuid::Uuid;

use crate::{
    admin::{
        AdminRole, NewAdmin,
        accounts::{
            AdminAccountAdminResponse, AdminAccountsRepository, AdminAccountsRepositoryError,
            model::{
                AdminAccountAdminListFilter, AdminAccountUserResponse, AdminCreatorResponse,
                AdminListQuery, AdminListResponse, UserListQuery, UserListResponse,
            },
        },
    },
    api::{PaginatedResponse, PaginationMeta},
    platform::{Password, PasswordError, validate_password},
    user::{
        model::UserListFilter,
        repository::{UserError, UserRepository},
    },
};

#[derive(Debug, thiserror::Error)]
pub enum AdminAccountsServiceError {
    #[error("user repository is none")]
    UserRepositoryNone,

    #[error("{0}")]
    InvalidQuery(String),

    #[error("admin phone already exists")]
    AlreadyExists,

    #[error("temporary password generation failed")]
    TemporaryPasswordGeneration(#[from] getrandom::Error),

    #[error("temporary password hashing failed")]
    PasswordHash(#[source] PasswordError),

    #[error("admin accounts repository failure")]
    Repository(#[source] AdminAccountsRepositoryError),

    #[error("user repository failure")]
    UserRepository(#[source] UserError),
}

fn normalize_search_pattern(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim();

        if value.is_empty() {
            None
        } else {
            Some(format!("%{}%", escape_like_literal(value)))
        }
    })
}

fn escape_like_literal(value: &str) -> String {
    value
        .replace('\\', r"\\")
        .replace('%', r"\%")
        .replace('_', r"\_")
}

pub struct AdminAccountsService {
    repository: AdminAccountsRepository,
    user_repository: Option<UserRepository>,
}

// 排除 0/O、1/I/l 等容易看错的字符
const TEMP_PASSWORD_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
const TEMP_PASSWORD_LEN: usize = 20;
fn generate_temporary_password(subject: &str) -> Result<String, getrandom::Error> {
    loop {
        let candidate = generate_temporary_password_candidate()?;
        if validate_password(&candidate, subject).is_ok() {
            return Ok(candidate);
        }
    }
}

fn generate_temporary_password_candidate() -> Result<String, getrandom::Error> {
    let alphabet_len = TEMP_PASSWORD_ALPHABET.len();

    // 只接受能被字符集长度整除的随机数范围，避免取模偏差
    let accepted_range = 256 - (256 % alphabet_len);

    let mut password = Vec::with_capacity(TEMP_PASSWORD_LEN);
    let mut random_bytes = [0u8; 32];

    while password.len() < TEMP_PASSWORD_LEN {
        getrandom::fill(&mut random_bytes)?;

        for byte in random_bytes {
            let value = usize::from(byte);

            if value >= accepted_range {
                continue;
            }

            password.push(TEMP_PASSWORD_ALPHABET[value % alphabet_len]);

            if password.len() == TEMP_PASSWORD_LEN {
                break;
            }
        }
    }

    Ok(String::from_utf8(password).expect("字符集必须全部是 ASCII"))
}

impl AdminAccountsService {
    pub fn new(
        repository: AdminAccountsRepository,
        user_repository: Option<UserRepository>,
    ) -> Self {
        Self {
            repository,
            user_repository,
        }
    }
    pub async fn provision(
        &self,
        id: Uuid,
        actor_display_name: &str,
        phone: &str,
        display_name: &str,
    ) -> Result<(AdminAccountAdminResponse, String), AdminAccountsServiceError> {
        // 1. 生成临时密码
        let temporary_password = generate_temporary_password(phone)?;
        // 2. 计算密码hash
        let password_hash = Password::parse(&temporary_password)
            .map_err(AdminAccountsServiceError::PasswordHash)?
            .hash()
            .await
            .map_err(AdminAccountsServiceError::PasswordHash)?;

        let admin = self
            .repository
            .create(NewAdmin {
                id: Uuid::now_v7(),
                phone: phone.into(),
                display_name: display_name.to_string(),
                password_hash,
                role: AdminRole::Admin,
                must_change_password: true,
                created_by_admin_id: Some(id),
            })
            .await
            .map_err(map_repository_error)?;

        Ok((
            AdminAccountAdminResponse {
                id: admin.id,
                phone: admin.phone,
                display_name: admin.display_name,
                role: admin.role,
                status: admin.status,
                created_at: admin.created_at,
                updated_at: admin.updated_at,
                created_by: Some(AdminCreatorResponse {
                    id,
                    display_name: actor_display_name.into(),
                }),
            },
            temporary_password,
        ))
    }

    pub async fn admin_list(
        &self,
        query: AdminListQuery,
    ) -> Result<AdminListResponse, AdminAccountsServiceError> {
        let page = query.pagination.page.unwrap_or(1);
        let page_size = query.pagination.page_size.unwrap_or(20);

        if page == 0 {
            return Err(AdminAccountsServiceError::InvalidQuery(
                "page must be at least 1".into(),
            ));
        }

        if !(1..=100).contains(&page_size) {
            return Err(AdminAccountsServiceError::InvalidQuery(
                "page_size must be between 1 and 100".into(),
            ));
        }

        let phone_pattern = normalize_search_pattern(query.filters.phone);
        let display_name_pattern = normalize_search_pattern(query.filters.display_name);

        let limit = i64::from(page_size);
        let offset = i64::from(page - 1) * limit;

        let filter = AdminAccountAdminListFilter {
            role: query.filters.role,
            phone_pattern,
            display_name_pattern,
            limit,
            offset,
        };

        let (records, total) = self
            .repository
            .admin_list(&filter)
            .await
            .map_err(map_repository_error)?;

        let items = records
            .into_iter()
            .map(|record| {
                let created_by = match (record.created_by_id, record.created_by_display_name) {
                    (Some(id), Some(display_name)) => {
                        Some(AdminCreatorResponse { id, display_name })
                    }
                    _ => None,
                };

                AdminAccountAdminResponse {
                    id: record.id,
                    phone: record.phone,
                    display_name: record.display_name,
                    role: record.role,
                    created_by,
                    status: record.status,
                    created_at: record.created_at,
                    updated_at: record.updated_at,
                }
            })
            .collect();

        let total_pages = if total == 0 {
            0
        } else {
            (total + i64::from(page_size) - 1) / i64::from(page_size)
        };

        Ok(PaginatedResponse {
            items,
            pagination: PaginationMeta {
                page,
                page_size,
                total,
                total_pages,
            },
        })
    }

    pub async fn user_list(
        &self,
        query: UserListQuery,
    ) -> Result<UserListResponse, AdminAccountsServiceError> {
        // 检查user_repo
        let user_repo = match &self.user_repository {
            Some(user_repository) => user_repository,
            None => {
                return Err(AdminAccountsServiceError::UserRepositoryNone);
            }
        };

        let page = query.pagination.page.unwrap_or(1);
        let page_size = query.pagination.page_size.unwrap_or(20);

        if page == 0 {
            return Err(AdminAccountsServiceError::InvalidQuery(
                "page must be at least 1".into(),
            ));
        }

        if !(1..=100).contains(&page_size) {
            return Err(AdminAccountsServiceError::InvalidQuery(
                "page_size must be between 1 and 100".into(),
            ));
        }

        let limit = i64::from(page_size);
        let offset = i64::from(page - 1) * limit;

        let display_name_pattern = normalize_search_pattern(query.filters.display_name);
        let phone_pattern = normalize_search_pattern(query.filters.phone);
        let email_pattern = normalize_search_pattern(query.filters.email);
        // TODO 注册时间

        let filter = UserListFilter {
            role: query.filters.role,
            phone_pattern,
            display_name_pattern,
            email_pattern,
            limit,
            offset,
        };

        let (records, total) = user_repo.user_list(&filter).await.map_err(map_user_error)?;

        let items = records
            .into_iter()
            .map(|record| AdminAccountUserResponse {
                id: record.id,
                phone: record.phone,
                email: record.email,
                display_name: record.display_name,
                student_role_cefr_level: record.cefr_level,
                student_role_english_variant: record.english_variant,
                avatar_url: record.avatar_url,
                created_at: record.created_at,
                updated_at: record.updated_at,
            })
            .collect();
        let total_pages = if total == 0 {
            0
        } else {
            (total + i64::from(page_size) - 1) / i64::from(page_size)
        };

        Ok(PaginatedResponse {
            items,
            pagination: PaginationMeta {
                page,
                page_size,
                total,
                total_pages,
            },
        })
    }
}

fn map_repository_error(error: AdminAccountsRepositoryError) -> AdminAccountsServiceError {
    match error {
        AdminAccountsRepositoryError::AlreadyExists => AdminAccountsServiceError::AlreadyExists,
        other => AdminAccountsServiceError::Repository(other),
    }
}

fn map_user_error(error: UserError) -> AdminAccountsServiceError {
    AdminAccountsServiceError::UserRepository(error)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn temporary_passwords_match_generation_contract() {
        let generated = (0..8)
            .map(|_| generate_temporary_password("13800138000").unwrap())
            .collect::<Vec<_>>();

        for password in &generated {
            assert_eq!(password.len(), TEMP_PASSWORD_LEN);
            assert!(
                password
                    .bytes()
                    .all(|byte| TEMP_PASSWORD_ALPHABET.contains(&byte))
            );
            assert!(validate_password(password, "13800138000").is_ok());
        }

        assert!(
            generated.iter().collect::<HashSet<_>>().len() > 1,
            "连续生成的临时密码不应全部相同"
        );
    }

    #[test]
    fn repository_conflict_becomes_domain_conflict() {
        assert!(matches!(
            map_repository_error(AdminAccountsRepositoryError::AlreadyExists),
            AdminAccountsServiceError::AlreadyExists
        ));
    }
}
