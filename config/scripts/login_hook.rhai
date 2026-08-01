// 登录回调钩子示例：在 OIDC 登录成功后可被调用
// inputs["claims"] 包含 ID Token claims
let claims = inputs["claims"];
#{
  sub: claims["sub"],
  hook: "login_hook executed"
}
