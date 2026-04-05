// Multi-Tenancy Module
//
// Provides organization isolation for SaaS deployments.
// Supports tenant quotas, resource limits, and usage tracking.
//
// # Features
//
// - **Tenant Isolation**: Complete data separation per organization
// - **Resource Quotas**: Configurable limits on documents, searches, storage
// - **Usage Tracking**: Monitor resource consumption per tenant
// - **Billing Metrics**: Track usage for invoicing
// - **Custom Domains**: Support for white-label deployments
//
// # Example
//
// ```rust,no_run
// use rustcode::multi_tenant::{TenantManager, TenantQuota, QuotaType, UsageMetric};
// use sqlx::PgPool;
//
// # async fn example() -> anyhow::Result<()> {
// let db_pool = PgPool::connect(&std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgresql://rustcode:changeme@localhost:5432/rustcode_test".to_string())).await?;
// let tenant_mgr = TenantManager::new(db_pool).await?;
//
// // Create new tenant
// let tenant = tenant_mgr.create_tenant(
//     "acme-corp",
//     "ACME Corporation",
//     TenantQuota::standard()
// ).await?;
//
// // Check quota before operation
// tenant_mgr.check_quota(&tenant.id, QuotaType::Documents).await?;
//
// // Track usage
// tenant_mgr.increment_usage(&tenant.id, UsageMetric::Documents(1)).await?;
// # Ok(())
// # }
// ```

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::collections::HashMap;

// ============================================================================
// Data Structures
// ============================================================================

// Tenant/Organization
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tenant {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub quota: TenantQuota,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub custom_domain: Option<String>,
}

// Resource quotas for a tenant
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantQuota {
    pub max_documents: i64,
    pub max_storage_mb: i64,
    pub max_searches_per_day: i64,
    pub max_api_keys: i64,
    pub max_webhooks: i64,
}

impl Default for TenantQuota {
    fn default() -> Self {
        Self::standard()
    }
}

impl TenantQuota {
    // Free tier quota
    pub fn free() -> Self {
        Self {
            max_documents: 100,
            max_storage_mb: 100,
            max_searches_per_day: 1000,
            max_api_keys: 2,
            max_webhooks: 1,
        }
    }

    // Standard paid tier
    pub fn standard() -> Self {
        Self {
            max_documents: 10000,
            max_storage_mb: 10240,
            max_searches_per_day: 100000,
            max_api_keys: 10,
            max_webhooks: 5,
        }
    }

    // Enterprise tier
    pub fn enterprise() -> Self {
        Self {
            max_documents: 1000000,
            max_storage_mb: 1048576,
            max_searches_per_day: 10000000,
            max_api_keys: 100,
            max_webhooks: 50,
        }
    }

    // Unlimited (for internal use)
    pub fn unlimited() -> Self {
        Self {
            max_documents: i64::MAX,
            max_storage_mb: i64::MAX,
            max_searches_per_day: i64::MAX,
            max_api_keys: i64::MAX,
            max_webhooks: i64::MAX,
        }
    }
}

// Current usage metrics for a tenant
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantUsage {
    pub tenant_id: String,
    pub document_count: i64,
    pub storage_mb: i64,
    pub searches_today: i64,
    pub api_key_count: i64,
    pub webhook_count: i64,
    pub last_updated: DateTime<Utc>,
}

// Usage metric types
#[derive(Debug, Clone)]
pub enum UsageMetric {
    Documents(i64),
    StorageMb(i64),
    Searches(i64),
    ApiKeys(i64),
    Webhooks(i64),
}

// Quota check result
#[derive(Debug, Clone)]
pub enum QuotaType {
    Documents,
    Storage,
    SearchesPerDay,
    ApiKeys,
    Webhooks,
}

// ============================================================================
// Tenant Manager
// ============================================================================

pub struct TenantManager {
    db_pool: PgPool,
}

impl TenantManager {
    // Create new tenant manager
    pub async fn new(db_pool: PgPool) -> Result<Self> {
        let manager = Self { db_pool };
        manager.init_tables().await?;
        Ok(manager)
    }

    // Initialize database tables
    async fn init_tables(&self) -> Result<()> {
        // Acquire a session-level advisory lock so that concurrent test threads
        // don't race on `CREATE TABLE IF NOT EXISTS` + `BIGSERIAL` sequence
        // creation, which triggers a `pg_type_typname_nsp_index` unique-
        // constraint violation inside Postgres.
        sqlx::query("SELECT pg_advisory_lock(7483921)")
            .execute(&self.db_pool)
            .await
            .context("Failed to acquire multi_tenant init lock")?;

        let result = self.init_tables_inner().await;

        let _ = sqlx::query("SELECT pg_advisory_unlock(7483921)")
            .execute(&self.db_pool)
            .await;

        result
    }

    async fn init_tables_inner(&self) -> Result<()> {
        // Organizations table
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS organizations (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                slug TEXT UNIQUE NOT NULL,
                max_documents BIGINT NOT NULL DEFAULT 10000,
                max_storage_mb BIGINT NOT NULL DEFAULT 10240,
                max_searches_per_day BIGINT NOT NULL DEFAULT 100000,
                max_api_keys BIGINT NOT NULL DEFAULT 10,
                max_webhooks BIGINT NOT NULL DEFAULT 5,
                enabled BOOLEAN NOT NULL DEFAULT TRUE,
                created_at TIMESTAMPTZ DEFAULT NOW(),
                custom_domain TEXT
            )
            "#,
        )
        .execute(&self.db_pool)
        .await
        .context("Failed to create organizations table")?;

        // Usage tracking table — use BIGINT for all counters to match the i64
        // Rust struct fields in TenantUsage; INTEGER (INT4) would cause a
        // "mismatched types INT8 vs INT4" decode error at runtime.
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS tenant_usage (
                tenant_id TEXT PRIMARY KEY,
                document_count BIGINT NOT NULL DEFAULT 0,
                storage_mb BIGINT NOT NULL DEFAULT 0,
                searches_today BIGINT NOT NULL DEFAULT 0,
                api_key_count BIGINT NOT NULL DEFAULT 0,
                webhook_count BIGINT NOT NULL DEFAULT 0,
                last_updated TIMESTAMPTZ DEFAULT NOW(),
                FOREIGN KEY (tenant_id) REFERENCES organizations(id) ON DELETE CASCADE
            )
            "#,
        )
        .execute(&self.db_pool)
        .await
        .context("Failed to create tenant_usage table")?;

        // Migrate any INTEGER (INT4) counter columns to BIGINT so they match
        // the i64 Rust fields in TenantUsage.  We use pg_attribute (the
        // internal catalog) rather than information_schema to avoid schema-
        // path ambiguity.  atttypid 23 = INT4; we only ALTER when the column
        // is still INT4.  An EXCEPTION handler makes the whole block idempotent
        // even if something unexpected happens mid-loop.
        sqlx::query(
            r#"
            DO $$
            DECLARE
                col  TEXT;
                toid OID;
            BEGIN
                -- Resolve the OID of tenant_usage in any schema on the
                -- current search_path (not just the first one), so the
                -- lookup works regardless of how the connection's search_path
                -- is configured.
                SELECT c.oid INTO toid
                FROM   pg_class c
                JOIN   pg_namespace n ON n.oid = c.relnamespace
                WHERE  c.relname = 'tenant_usage'
                  AND  n.nspname = ANY(current_schemas(false))
                LIMIT  1;

                IF toid IS NULL THEN
                    RETURN;  -- table not visible yet, nothing to migrate
                END IF;

                FOREACH col IN ARRAY ARRAY[
                    'document_count','storage_mb','searches_today',
                    'api_key_count','webhook_count'
                ]
                LOOP
                    -- atttypid 23 is INT4 (integer)
                    IF EXISTS (
                        SELECT 1
                        FROM   pg_attribute
                        WHERE  attrelid = toid
                          AND  attname  = col
                          AND  atttypid = 23
                          AND  attnum   > 0
                          AND  NOT attisdropped
                    ) THEN
                        EXECUTE format(
                            'ALTER TABLE tenant_usage ALTER COLUMN %I TYPE BIGINT',
                            col
                        );
                    END IF;
                END LOOP;
            EXCEPTION WHEN OTHERS THEN
                -- Swallow any transient error (e.g. concurrent ALTER) so the
                -- migration never blocks application startup.
                RAISE WARNING 'tenant_usage BIGINT migration skipped: %', SQLERRM;
            END
            $$
            "#,
        )
        .execute(&self.db_pool)
        .await
        .context("Failed to migrate tenant_usage counter columns to BIGINT")?;

        // Daily usage history — BIGINT for consistency with TenantUsage fields
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS tenant_usage_history (
                id BIGSERIAL PRIMARY KEY,
                tenant_id TEXT NOT NULL,
                date DATE NOT NULL,
                documents_created BIGINT NOT NULL DEFAULT 0,
                searches_performed BIGINT NOT NULL DEFAULT 0,
                storage_mb BIGINT NOT NULL DEFAULT 0,
                FOREIGN KEY (tenant_id) REFERENCES organizations(id) ON DELETE CASCADE,
                UNIQUE(tenant_id, date)
            )
            "#,
        )
        .execute(&self.db_pool)
        .await
        .context("Failed to create tenant_usage_history table")?;

        Ok(())
    }

    // Create new tenant
    pub async fn create_tenant(
        &self,
        slug: &str,
        name: &str,
        quota: TenantQuota,
    ) -> Result<Tenant> {
        let id = uuid::Uuid::new_v4().to_string();
        let created_at_dt = Utc::now();

        // Use NOW() default for created_at (TIMESTAMPTZ column) — no binding needed.
        sqlx::query(
            r#"
            INSERT INTO organizations (
                id, name, slug, max_documents, max_storage_mb, max_searches_per_day,
                max_api_keys, max_webhooks, enabled
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, TRUE)
            "#,
        )
        .bind(&id)
        .bind(name)
        .bind(slug)
        .bind(quota.max_documents)
        .bind(quota.max_storage_mb)
        .bind(quota.max_searches_per_day)
        .bind(quota.max_api_keys)
        .bind(quota.max_webhooks)
        .execute(&self.db_pool)
        .await
        .context("Failed to create tenant")?;

        // Initialize usage tracking — last_updated uses NOW() default.
        sqlx::query(
            r#"
            INSERT INTO tenant_usage (tenant_id)
            VALUES ($1)
            "#,
        )
        .bind(&id)
        .execute(&self.db_pool)
        .await?;

        Ok(Tenant {
            id,
            name: name.to_string(),
            slug: slug.to_string(),
            quota,
            enabled: true,
            created_at: created_at_dt,
            custom_domain: None,
        })
    }

    // Get tenant by ID
    pub async fn get_tenant(&self, tenant_id: &str) -> Result<Option<Tenant>> {
        let row = sqlx::query_as::<
            _,
            (
                String,
                String,
                String,
                i64,
                i64,
                i64,
                i64,
                i64,
                bool,
                DateTime<Utc>,
                Option<String>,
            ),
        >(
            r#"
            SELECT id, name, slug, max_documents, max_storage_mb, max_searches_per_day,
                   max_api_keys, max_webhooks, enabled, created_at, custom_domain
            FROM organizations
            WHERE id = $1
            "#,
        )
        .bind(tenant_id)
        .fetch_optional(&self.db_pool)
        .await?;

        if let Some((
            id,
            name,
            slug,
            max_docs,
            max_storage,
            max_searches,
            max_keys,
            max_hooks,
            enabled,
            created,
            domain,
        )) = row
        {
            Ok(Some(Tenant {
                id,
                name,
                slug,
                quota: TenantQuota {
                    max_documents: max_docs,
                    max_storage_mb: max_storage,
                    max_searches_per_day: max_searches,
                    max_api_keys: max_keys,
                    max_webhooks: max_hooks,
                },
                enabled,
                created_at: created,
                custom_domain: domain,
            }))
        } else {
            Ok(None)
        }
    }

    // Get tenant by slug
    pub async fn get_tenant_by_slug(&self, slug: &str) -> Result<Option<Tenant>> {
        let row = sqlx::query_scalar::<_, String>("SELECT id FROM organizations WHERE slug = $1")
            .bind(slug)
            .fetch_optional(&self.db_pool)
            .await?;

        if let Some(id) = row {
            self.get_tenant(&id).await
        } else {
            Ok(None)
        }
    }

    // Get tenant by API key
    pub async fn get_tenant_by_key(&self, api_key_hash: &str) -> Result<Option<Tenant>> {
        let tenant_id =
            sqlx::query_scalar::<_, String>("SELECT tenant_id FROM api_keys WHERE key_hash = $1")
                .bind(api_key_hash)
                .fetch_optional(&self.db_pool)
                .await?;

        if let Some(id) = tenant_id {
            self.get_tenant(&id).await
        } else {
            Ok(None)
        }
    }

    // Get current usage for tenant
    pub async fn get_usage(&self, tenant_id: &str) -> Result<TenantUsage> {
        // All counter columns are BIGINT (i64); api_key_count and webhook_count
        // are stored as BIGINT too (schema changed from INTEGER to match Rust types).
        let row = sqlx::query_as::<_, (i64, i64, i64, i64, i64, DateTime<Utc>)>(
            r#"
            SELECT document_count, storage_mb, searches_today, api_key_count, webhook_count, last_updated
            FROM tenant_usage
            WHERE tenant_id = $1
            "#,
        )
        .bind(tenant_id)
        .fetch_one(&self.db_pool)
        .await
        .context("Failed to fetch tenant usage")?;

        Ok(TenantUsage {
            tenant_id: tenant_id.to_string(),
            document_count: row.0,
            storage_mb: row.1,
            searches_today: row.2,
            api_key_count: row.3,
            webhook_count: row.4,
            last_updated: row.5,
        })
    }

    // Check if operation would exceed quota
    pub async fn check_quota(&self, tenant_id: &str, quota_type: QuotaType) -> Result<()> {
        let tenant = self
            .get_tenant(tenant_id)
            .await?
            .ok_or_else(|| anyhow!("Tenant not found"))?;

        if !tenant.enabled {
            return Err(anyhow!("Tenant is disabled"));
        }

        let usage = self.get_usage(tenant_id).await?;

        match quota_type {
            QuotaType::Documents => {
                if usage.document_count >= tenant.quota.max_documents {
                    return Err(anyhow!(
                        "Document quota exceeded ({}/{})",
                        usage.document_count,
                        tenant.quota.max_documents
                    ));
                }
            }
            QuotaType::Storage => {
                if usage.storage_mb >= tenant.quota.max_storage_mb {
                    return Err(anyhow!(
                        "Storage quota exceeded ({} MB / {} MB)",
                        usage.storage_mb,
                        tenant.quota.max_storage_mb
                    ));
                }
            }
            QuotaType::SearchesPerDay => {
                if usage.searches_today >= tenant.quota.max_searches_per_day {
                    return Err(anyhow!(
                        "Daily search quota exceeded ({}/{})",
                        usage.searches_today,
                        tenant.quota.max_searches_per_day
                    ));
                }
            }
            QuotaType::ApiKeys => {
                if usage.api_key_count >= tenant.quota.max_api_keys {
                    return Err(anyhow!(
                        "API key quota exceeded ({}/{})",
                        usage.api_key_count,
                        tenant.quota.max_api_keys
                    ));
                }
            }
            QuotaType::Webhooks => {
                if usage.webhook_count >= tenant.quota.max_webhooks {
                    return Err(anyhow!(
                        "Webhook quota exceeded ({}/{})",
                        usage.webhook_count,
                        tenant.quota.max_webhooks
                    ));
                }
            }
        }

        Ok(())
    }

    // Increment usage counter
    pub async fn increment_usage(&self, tenant_id: &str, metric: UsageMetric) -> Result<()> {
        let (field, value) = match metric {
            UsageMetric::Documents(n) => ("document_count", n),
            UsageMetric::StorageMb(n) => ("storage_mb", n),
            UsageMetric::Searches(n) => ("searches_today", n),
            UsageMetric::ApiKeys(n) => ("api_key_count", n),
            UsageMetric::Webhooks(n) => ("webhook_count", n),
        };

        let query = format!(
            "UPDATE tenant_usage SET {} = {} + $1, last_updated = NOW() WHERE tenant_id = $2",
            field, field
        );

        sqlx::query(&query)
            .bind(value)
            .bind(tenant_id)
            .execute(&self.db_pool)
            .await?;

        Ok(())
    }

    // Decrement usage counter
    pub async fn decrement_usage(&self, tenant_id: &str, metric: UsageMetric) -> Result<()> {
        let (field, value) = match metric {
            UsageMetric::Documents(n) => ("document_count", n),
            UsageMetric::StorageMb(n) => ("storage_mb", n),
            UsageMetric::Searches(n) => ("searches_today", n),
            UsageMetric::ApiKeys(n) => ("api_key_count", n),
            UsageMetric::Webhooks(n) => ("webhook_count", n),
        };

        let query = format!(
            "UPDATE tenant_usage SET {} = GREATEST(0, {} - $1), last_updated = NOW() WHERE tenant_id = $2",
            field, field
        );

        sqlx::query(&query)
            .bind(value)
            .bind(tenant_id)
            .execute(&self.db_pool)
            .await?;

        Ok(())
    }

    // Reset daily search counter (call this daily)
    pub async fn reset_daily_searches(&self) -> Result<u64> {
        let result =
            sqlx::query("UPDATE tenant_usage SET searches_today = 0, last_updated = NOW()")
                .execute(&self.db_pool)
                .await?;

        Ok(result.rows_affected())
    }

    // Update tenant quota
    pub async fn update_quota(&self, tenant_id: &str, quota: TenantQuota) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE organizations
            SET max_documents = $1, max_storage_mb = $2, max_searches_per_day = $3,
                max_api_keys = $4, max_webhooks = $5
            WHERE id = $6
            "#,
        )
        .bind(quota.max_documents)
        .bind(quota.max_storage_mb)
        .bind(quota.max_searches_per_day)
        .bind(quota.max_api_keys)
        .bind(quota.max_webhooks)
        .bind(tenant_id)
        .execute(&self.db_pool)
        .await?;

        Ok(())
    }

    // Enable/disable tenant
    pub async fn set_tenant_enabled(&self, tenant_id: &str, enabled: bool) -> Result<()> {
        sqlx::query("UPDATE organizations SET enabled = $1 WHERE id = $2")
            .bind(enabled)
            .bind(tenant_id)
            .execute(&self.db_pool)
            .await?;

        Ok(())
    }

    // List all tenants
    pub async fn list_tenants(&self) -> Result<Vec<Tenant>> {
        let rows = sqlx::query_as::<
            _,
            (
                String,
                String,
                String,
                i64,
                i64,
                i64,
                i64,
                i64,
                bool,
                DateTime<Utc>,
                Option<String>,
            ),
        >(
            r#"
            SELECT id, name, slug, max_documents, max_storage_mb, max_searches_per_day,
                   max_api_keys, max_webhooks, enabled, created_at, custom_domain
            FROM organizations
            ORDER BY created_at DESC
            "#,
        )
        .fetch_all(&self.db_pool)
        .await?;

        let mut tenants = Vec::new();
        for (
            id,
            name,
            slug,
            max_docs,
            max_storage,
            max_searches,
            max_keys,
            max_hooks,
            enabled,
            created,
            domain,
        ) in rows
        {
            tenants.push(Tenant {
                id,
                name,
                slug,
                quota: TenantQuota {
                    max_documents: max_docs,
                    max_storage_mb: max_storage,
                    max_searches_per_day: max_searches,
                    max_api_keys: max_keys,
                    max_webhooks: max_hooks,
                },
                enabled,
                created_at: created,
                custom_domain: domain,
            });
        }

        Ok(tenants)
    }

    // Get billing metrics for tenant
    pub async fn get_billing_metrics(
        &self,
        tenant_id: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<HashMap<String, i64>> {
        let rows = sqlx::query_as::<_, (i64, i64, i64)>(
            r#"
            SELECT
                SUM(documents_created) as total_documents,
                SUM(searches_performed) as total_searches,
                AVG(storage_mb) as avg_storage
            FROM tenant_usage_history
            WHERE tenant_id = $1 AND date BETWEEN $2 AND $3
            "#,
        )
        .bind(tenant_id)
        .bind(start.format("%Y-%m-%d").to_string())
        .bind(end.format("%Y-%m-%d").to_string())
        .fetch_one(&self.db_pool)
        .await?;

        let mut metrics = HashMap::new();
        metrics.insert("total_documents".to_string(), rows.0);
        metrics.insert("total_searches".to_string(), rows.1);
        metrics.insert("avg_storage_mb".to_string(), rows.2);

        Ok(metrics)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    async fn setup_test_db() -> PgPool {
        PgPool::connect(&std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgresql://rustcode:changeme@localhost:5432/rustcode_test".to_string()
        }))
        .await
        .unwrap()
    }

    // Generate a unique slug for each test run so parallel tests don't collide
    // on the `organizations_slug_key` unique constraint.
    fn unique_slug(base: &str) -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos();
        // Include the thread id for extra uniqueness when many tests run at once.
        let tid = format!("{:?}", std::thread::current().id());
        let tid_hash: u32 = tid.bytes().fold(0u32, |acc, b| acc.wrapping_add(b as u32));
        format!("{}-{}-{}", base, nanos, tid_hash)
    }

    #[tokio::test]
    async fn test_create_tenant() {
        let pool = setup_test_db().await;
        let manager = TenantManager::new(pool).await.unwrap();

        let slug = unique_slug("test-org");
        let tenant = manager
            .create_tenant(&slug, "Test Organization", TenantQuota::standard())
            .await
            .unwrap();

        assert_eq!(tenant.slug, slug);
        assert_eq!(tenant.name, "Test Organization");
        assert!(tenant.enabled);
    }

    #[tokio::test]
    async fn test_quota_check() {
        let pool = setup_test_db().await;
        let manager = TenantManager::new(pool).await.unwrap();

        let slug = unique_slug("quota-org");
        let tenant = manager
            .create_tenant(&slug, "Test Org", TenantQuota::free())
            .await
            .unwrap();

        // Should pass initially
        manager
            .check_quota(&tenant.id, QuotaType::Documents)
            .await
            .unwrap();

        // Increment to limit
        for _ in 0..100 {
            manager
                .increment_usage(&tenant.id, UsageMetric::Documents(1))
                .await
                .unwrap();
        }

        // Should now fail
        let result = manager.check_quota(&tenant.id, QuotaType::Documents).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_usage_tracking() {
        let pool = setup_test_db().await;
        let manager = TenantManager::new(pool).await.unwrap();

        let slug = unique_slug("usage-org");
        let tenant = manager
            .create_tenant(&slug, "Test Org", TenantQuota::standard())
            .await
            .unwrap();

        // Increment documents
        manager
            .increment_usage(&tenant.id, UsageMetric::Documents(5))
            .await
            .unwrap();

        // Increment searches
        manager
            .increment_usage(&tenant.id, UsageMetric::Searches(10))
            .await
            .unwrap();

        let usage = manager.get_usage(&tenant.id).await.unwrap();
        assert_eq!(usage.document_count, 5);
        assert_eq!(usage.searches_today, 10);

        // Decrement
        manager
            .decrement_usage(&tenant.id, UsageMetric::Documents(2))
            .await
            .unwrap();

        let usage = manager.get_usage(&tenant.id).await.unwrap();
        assert_eq!(usage.document_count, 3);
    }
}
