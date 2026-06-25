use ar_github::GitHubAppJwt;
use base64::prelude::{Engine as _, BASE64_URL_SAFE_NO_PAD};
use jsonwebtoken::{decode_header, Algorithm};
use serde_json::Value;

const TEST_PRIVATE_KEY: &str = r#"-----BEGIN PRIVATE KEY-----
MIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQCTj+cMup5fx8o5
NmzvputlN0EN+2Q3bOH4PsnDUZtkx9Ycpa0l135cnnDWqZ5OHaS8IwpUl75MnBpe
WKYuI3ZNxo4uYdvuU9xogMjlyfOSBtjlGxxZtOSq+ucoXynBhVPymnwOkM3BiHKu
b729l0rmps8tLbFl44qDRwvrP5Gzfaiu7Wdx6M4CN9wSPKdpWIl8ZxukaQDLaKuq
UY3FrOkkYzyRwdBshJEUIH0es84wjZTy9aqAu9+V0duF41qwmNn1ZPf2/gVR2p+H
hpylXQ80RlVWNRvd8ftm33STEBu9W/s/PWgmWLAHtLd20GXGmJECyzl2AO3zmbPb
Va4pSGTFAgMBAAECggEAB5YqxsX7tBNkibILM/v1xS1r9dH1TPgzk8PpT+Ssw9J5
9U7e3KNRjfPuGCCa0OEdEBwQMuelb5uPcpxT9SP6uB9mPflY7QbqhRK3T7LsQNBK
a5F90OA56QgRGqDR5f1QFJjt0siN/yOY4rdAO1EIiSN/f48GZxcz5DiixlnfHfEi
FTFKTeS3ZrtvxKI8rA3xIlqXTLMlpI8PNAorlNfshURziMnu8Qb+CZCF9HzLFWx/
59YwsHCc65Gzt7l//tCsP/J4ccC2/yudp1qM4wwnmg2HEac+BSXiIkDC+sNC9iOz
zv8Pedpg+UE396kGSuiOCiCiiVIgMtB+7yeGyRyo/QKBgQDInhpxP33Q1GeEljRv
tMKzuFZ/rsjuk1TEPNQ1vDTCoSsDS+WO6dwQJqHz6wtXJ6TTWSxZGiIfZRo6D6+x
5p0z/7nchO9GTCUm2w5vmRc7QPR2LSteQD4rS7FYxaEJK3VYrRbsVD19N5+WmYQS
1BqSdJLU8m+hHleAzk/dVocKEwKBgQC8TE1zC/0pvkrC+mGeg4iVdV+o9Gr86KU5
Rcr4boFs/NxdpcifqmhZT1Orum9WbJjleFUXqT4/RwQEMNxMeFDugRckV9vaK/I9
ryWaw4r1te3X09PQPjCazV7tXbZtaGjHbPt0CzwrM2Uj+MkXEvZ47YdMkVQDzGal
LYojZ2QwxwKBgQCgk/RkW0awpaMqPDRCiXQi+FS8JMIOPH1oCaxuSnQMqYZNQC3B
o6ySfOm63JMaMtA6nzDhLQrNCFbizHV8NzPzUCiLuxfus7o1DLfUWbx/oDFMekEx
UsiwTQjqHl4UgeWOkhU8Gv2QwboKiFxv2bhfK9HvgxIY67lsjzbb2ouCYwKBgQCt
5RmxoeTPZDnPD0SQC5Fb1eXeNruHjaLo3ZexDvzgFjW9FpnNOqbbs/LlqWuhOPGL
GjIbg+VEsuT2qdkJliPH39frbGhReDxi0xm7b9MpUDazF6GJs6VXkUqVk/Ih4EbD
xdYDOUz76J8DNYMgmBafX12QTLZ2O5KgAQqN37IbiQKBgHfAq1aSUpdAoyCcL7az
LwizBI1dITEg7t4aGVzA9ByJf7KHHPRGEKdXqW4tAixWO5pjkaxoxqnhPOeV7tyh
E7WVGvUKDVt8xSzWQ6sJm2EOLwwU+lbXYDpiGalrthdB/bBZRumLEVfewFUnvJUM
AYKARgALZ8eYvQborOT7wnhm
-----END PRIVATE KEY-----"#;

#[test]
fn app_jwt_uses_rs256_app_id_issued_at_and_ten_minute_expiry() {
    let signer = GitHubAppJwt::from_rsa_pem(12345, TEST_PRIVATE_KEY.as_bytes()).expect("signer");

    let jwt = signer.jwt_at_unix(1_700_000_000).expect("jwt");

    let header = decode_header(&jwt).expect("header");
    assert_eq!(header.alg, Algorithm::RS256);

    let payload = jwt.split('.').nth(1).expect("payload segment");
    let decoded = BASE64_URL_SAFE_NO_PAD
        .decode(payload)
        .expect("payload base64");
    let claims: Value = serde_json::from_slice(&decoded).expect("claims json");
    assert_eq!(claims["iss"], "12345");
    assert_eq!(claims["iat"], 1_700_000_000);
    assert_eq!(claims["exp"], 1_700_000_600);
}
