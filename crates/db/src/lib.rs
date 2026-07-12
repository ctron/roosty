use roost_core::{Result, RoostError};
use sea_orm::{Database, DatabaseConnection};

pub type DbConnection = DatabaseConnection;

pub async fn connect(database_url: &str) -> Result<DbConnection> {
    Database::connect(database_url)
        .await
        .map_err(|error| RoostError::Database(error.to_string()))
}
