-- Settings rename used to PATCH name only while the host defaulted missing kind to
-- "store", flipping warehouses. Restore obvious warehouse rows so import qty classifies correctly.
UPDATE stock_locations
SET kind = 'warehouse'
WHERE kind != 'warehouse'
  AND (
    lower(slug) LIKE '%warehouse%'
    OR lower(name) LIKE '%warehouse%'
  );

-- Bootstrap defaults: id 1 warehouse, id 2 store when those rows still exist.
UPDATE stock_locations SET kind = 'warehouse' WHERE id = 1 AND slug IN ('warehouse', 'main-warehouse');
UPDATE stock_locations SET kind = 'store' WHERE id = 2 AND slug IN ('shop-floor', 'store', 'main-store');
