CREATE INDEX IF NOT EXISTS idx_deployment_logs_deployment_id_id
    ON deployment_logs(deployment_id, id ASC);
