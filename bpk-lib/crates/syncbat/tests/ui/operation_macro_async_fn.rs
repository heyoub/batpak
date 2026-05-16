#[syncbat::operation(
    descriptor = ECHO,
    name = "echo",
    effect = Compute,
    input_schema = "schema.echo.input.v1",
    output_schema = "schema.echo.output.v1",
    receipt_kind = "receipt.echo.v1"
)]
async fn echo(_input: &[u8], _cx: &mut syncbat::Cx<'_>) -> syncbat::HandlerResult {
    Ok(Vec::new())
}

fn main() {}
