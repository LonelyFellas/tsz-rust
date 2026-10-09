WITH wallet_totals AS (SELECT w.id,w.balance,COALESCE(sum(e.delta),0) AS ledger
        FROM coin_wallets w LEFT JOIN coin_entries e ON e.wallet_id=w.id GROUP BY w.id),
    running AS (SELECT balance_after,sum(delta) OVER (PARTITION BY wallet_id ORDER BY id) AS expected FROM coin_entries),
    operations AS (SELECT o.kind,count(e.id) AS n,COALESCE(sum(e.delta),0) AS delta,
        count(*) FILTER (WHERE e.delta>0) AS positives,count(*) FILTER (WHERE e.delta<0) AS negatives
        FROM coin_operations o LEFT JOIN coin_entries e ON e.operation_id=o.id GROUP BY o.id)
    SELECT (SELECT count(*) FROM wallet_totals WHERE balance<>ledger) AS wallet_mismatches,
        (SELECT count(*) FROM running WHERE balance_after<>expected) AS running_balance_mismatches,
        (SELECT count(*) FROM operations WHERE NOT (
            (kind='credit' AND n=1 AND positives=1) OR
            (kind IN ('debit','account_closure_forfeit') AND n=1 AND negatives=1) OR
            (kind='transfer' AND n=2 AND positives=1 AND negatives=1 AND delta=0))) AS operation_mismatches,
        (SELECT COALESCE(sum(balance),0)::text FROM coin_wallets) AS total_balance,
        (SELECT COALESCE(sum(delta),0)::text FROM operations WHERE kind<>'transfer') AS net_issuance
