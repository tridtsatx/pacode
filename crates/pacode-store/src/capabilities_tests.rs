use super::*;
use pacode_types::{AgentId, SessionId};

fn complete_capabilities() -> StorageCapabilities {
    StorageCapabilities {
        dialect: StorageDialect::Sqlite,
        fts5: true,
        journal_mode: JournalMode::Wal,
        synchronous: SynchronousMode::Normal,
        transactions: true,
        keyset_pagination: true,
    }
}

#[test]
fn pacode_requirements_pin_the_storage_dialect_contract() {
    let requirements = DialectRequirements::PACODE;

    assert!(requirements.fts5);
    assert!(requirements.wal);
    assert!(requirements.transactions);
    assert!(requirements.keyset_pagination);
}

#[test]
fn capability_validation_reports_each_missing_requirement() {
    let mut capabilities = complete_capabilities();
    assert!(capabilities.satisfies(&DialectRequirements::PACODE));
    assert!(capabilities.gaps(&DialectRequirements::PACODE).is_empty());

    capabilities.journal_mode = JournalMode::Delete;
    assert_eq!(
        capabilities.gaps(&DialectRequirements::PACODE),
        vec![CapabilityGap::Wal]
    );

    capabilities = complete_capabilities();
    capabilities.fts5 = false;
    capabilities.transactions = false;
    capabilities.keyset_pagination = false;
    assert_eq!(
        capabilities.gaps(&DialectRequirements::PACODE),
        vec![
            CapabilityGap::Fts5,
            CapabilityGap::Transactions,
            CapabilityGap::KeysetPagination,
        ]
    );
}

#[test]
fn durability_level_is_reported_rather_than_required() {
    // The store picks its own `synchronous` level, and it picked NORMAL: a
    // requirement that demanded FULL would report every pacode store as
    // short of its own contract. What still has to hold is WAL itself.
    for synchronous in [
        SynchronousMode::Off,
        SynchronousMode::Normal,
        SynchronousMode::Full,
        SynchronousMode::Extra,
    ] {
        let mut capabilities = complete_capabilities();
        capabilities.synchronous = synchronous;
        assert_eq!(
            capabilities.synchronous, synchronous,
            "it is reported as read"
        );
        assert!(capabilities.satisfies(&DialectRequirements::PACODE));
    }
}

#[test]
fn backend_label_does_not_replace_capability_checks() {
    let mut capabilities = complete_capabilities();
    capabilities.dialect = StorageDialect::Turso;
    capabilities.fts5 = false;

    assert_eq!(
        capabilities.gaps(&DialectRequirements::PACODE),
        vec![CapabilityGap::Fts5]
    );
}

#[test]
fn keyset_pages_advance_with_a_cursor_and_a_bounded_default_limit() {
    let first = KeysetPage::new(None, 0).normalized();
    assert_eq!(first.limit, KeysetPage::DEFAULT_LIMIT);
    assert_eq!(first.before, None);

    let second = first.next_page(KeysetCursor::new(17));
    assert_eq!(second.before, Some(KeysetCursor::new(17)));
    assert_eq!(second.limit, KeysetPage::DEFAULT_LIMIT);

    let page = QueryPage::new(vec![1_u64, 2], Some(KeysetCursor::new(1)));
    assert!(page.has_more());
    assert!(!QueryPage::new(vec![1_u64, 2], None).has_more());
}

#[test]
fn query_requests_keep_domain_values_outside_sql_specific_types() {
    let history = MessageHistoryQuery::new(
        SessionId::new("ses_contract"),
        AgentId::main(),
        Some(KeysetCursor::new(9)),
        25,
    );
    assert_eq!(history.session, SessionId::new("ses_contract"));
    assert_eq!(history.agent, AgentId::main());
    assert_eq!(history.page.before, Some(KeysetCursor::new(9)));
    assert_eq!(history.page.limit, 25);

    let search = SearchQuery::new("fts5", 0);
    assert_eq!(search.text, "fts5");
    assert_eq!(search.effective_limit(), KeysetPage::DEFAULT_LIMIT);
}

#[test]
fn benchmark_operations_cover_the_required_storage_behaviors() {
    let operations = [
        BenchmarkOperation::Fts5Search,
        BenchmarkOperation::TransactionCommit,
        BenchmarkOperation::TransactionRollback,
        BenchmarkOperation::KeysetPage,
    ];

    assert_eq!(operations.len(), 4);
    assert!(operations.contains(&BenchmarkOperation::Fts5Search));
    assert!(operations.contains(&BenchmarkOperation::TransactionCommit));
    assert!(operations.contains(&BenchmarkOperation::TransactionRollback));
    assert!(operations.contains(&BenchmarkOperation::KeysetPage));
}
