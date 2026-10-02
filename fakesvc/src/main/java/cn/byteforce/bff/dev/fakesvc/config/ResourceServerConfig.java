package com.example.fakesvc.config;

import org.springframework.context.annotation.Bean;
import org.springframework.context.annotation.Configuration;
import org.springframework.security.config.annotation.web.builders.HttpSecurity;
import org.springframework.security.config.annotation.web.configuration.EnableWebSecurity;
import org.springframework.security.config.http.SessionCreationPolicy;
import org.springframework.security.web.SecurityFilterChain;

/**
 * Resource Server 安全配置。
 * <p>
 * fakesvc 作为 iam 的资源服务器，验证 JWT token。
 * REST API 端点需要有效的 Bearer token，
 * WebSocket / SSE / Actuator / Swagger 端点放行（便于测试和运维）。
 */
@Configuration
@EnableWebSecurity
public class ResourceServerConfig {

    @Bean
    public SecurityFilterChain resourceServerFilterChain(HttpSecurity http) throws Exception {
        http
            // 无状态 — 不创建 session
            .sessionManagement(session -> session
                .sessionCreationPolicy(SessionCreationPolicy.STATELESS)
            )
            .authorizeHttpRequests(authorize -> authorize
                // Swagger / OpenAPI 端点放行
                .requestMatchers(
                    "/swagger-ui/**",
                    "/swagger-ui.html",
                    "/v3/api-docs/**"
                ).permitAll()
                // Actuator 健康检查放行
                .requestMatchers("/actuator/**").permitAll()
                // WebSocket 端点放行（WS 握手难以携带 Bearer token）
                .requestMatchers("/ws/**").permitAll()
                // SSE 端点放行
                .requestMatchers("/sse/**").permitAll()
                // 错误页面放行（避免 404 → /error 时触发认证）
                .requestMatchers("/error").permitAll()
                // REST API 需要 JWT 认证
                .requestMatchers("/api/**").authenticated()
                .anyRequest().authenticated()
            )
            .oauth2ResourceServer(oauth2 -> oauth2
                .jwt(jwt -> {
                    // JWT 配置由 application.yml 中的
                    // spring.security.oauth2.resourceserver.jwt.issuer-uri 提供
                })
            )
            // 禁用 CSRF（资源服务器使用 Bearer token）
            .csrf(csrf -> csrf.disable());

        return http.build();
    }
}
