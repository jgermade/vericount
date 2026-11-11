//! The migration command can make the server's login role a member of
//! vericounts_app.

use sqlx::PgPool;
use vericount_server::grant_app_role;

#[sqlx::test]
async fn the_app_role_is_granted_once(pool: PgPool) {
    // Roles belong to the whole cluster, not to the test database.
    let role = format!(
        "vc_test_{}",
        &vericount_server::access::sessions::new_token()[..12]
    );
    sqlx::query(&format!("CREATE ROLE {role} LOGIN"))
        .execute(&pool)
        .await
        .unwrap();

    let result = async {
        assert_eq!(grant_app_role(&pool, &role).await, Ok(true));
        let member: bool = sqlx::query_scalar("SELECT pg_has_role($1, 'vericounts_app', 'MEMBER')")
            .bind(&role)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(member);
        assert_eq!(grant_app_role(&pool, &role).await, Ok(false));

        assert!(grant_app_role(&pool, "vc_no_such_role_x").await.is_err());
        for invalid in [
            "",
            "Upper",
            "1st",
            "a;DROP TABLE users",
            "with space",
            "a-b",
        ] {
            assert!(grant_app_role(&pool, invalid).await.is_err(), "{invalid}");
        }
    };
    result.await;

    sqlx::query(&format!("DROP ROLE {role}"))
        .execute(&pool)
        .await
        .unwrap();
}
