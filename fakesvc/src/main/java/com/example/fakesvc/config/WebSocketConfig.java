package com.example.fakesvc.config;

import com.example.fakesvc.controller.WsChatHandler;
import com.example.fakesvc.controller.WsClockHandler;
import com.example.fakesvc.controller.WsEchoHandler;
import com.example.fakesvc.controller.WsWelcomeHandler;
import org.springframework.context.annotation.Configuration;
import org.springframework.web.socket.config.annotation.EnableWebSocket;
import org.springframework.web.socket.config.annotation.WebSocketConfigurer;
import org.springframework.web.socket.config.annotation.WebSocketHandlerRegistry;

/**
 * WebSocket 路由注册。
 */
@Configuration
@EnableWebSocket
public class WebSocketConfig implements WebSocketConfigurer {

    private final WsEchoHandler wsEchoHandler;
    private final WsClockHandler wsClockHandler;
    private final WsChatHandler wsChatHandler;
    private final WsWelcomeHandler wsWelcomeHandler;

    public WebSocketConfig(WsEchoHandler wsEchoHandler,
                           WsClockHandler wsClockHandler,
                           WsChatHandler wsChatHandler,
                           WsWelcomeHandler wsWelcomeHandler) {
        this.wsEchoHandler = wsEchoHandler;
        this.wsClockHandler = wsClockHandler;
        this.wsChatHandler = wsChatHandler;
        this.wsWelcomeHandler = wsWelcomeHandler;
    }

    @Override
    public void registerWebSocketHandlers(WebSocketHandlerRegistry registry) {
        registry.addHandler(wsWelcomeHandler, "/ws")
            .setAllowedOrigins("*");
        registry.addHandler(wsEchoHandler, "/ws/echo")
            .setAllowedOrigins("*");
        registry.addHandler(wsClockHandler, "/ws/clock")
            .setAllowedOrigins("*");
        registry.addHandler(wsChatHandler, "/ws/chat")
            .setAllowedOrigins("*");
    }
}
