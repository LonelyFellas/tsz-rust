-- Serialize with writers before checking: never discard a ledger during rollback.
LOCK TABLE coin_wallets, coin_operations, coin_entries IN ACCESS EXCLUSIVE MODE;
DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM coin_operations) OR EXISTS (SELECT 1 FROM coin_entries)
       OR EXISTS (SELECT 1 FROM coin_wallets WHERE balance <> 0 OR status <> 'open') THEN
        RAISE EXCEPTION 'coins rollback refused: ledger or lifecycle evidence exists';
    END IF;
END $$;
DROP TABLE coin_entries;
DROP TABLE coin_operations;
DROP TABLE coin_wallets;
