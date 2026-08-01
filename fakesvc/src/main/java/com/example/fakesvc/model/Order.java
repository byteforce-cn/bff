package com.example.fakesvc.model;

public class Order {

    private Long id;
    private String userId;
    private String product;
    private double amount;
    private String status;

    public Order() {
    }

    public Order(Long id, String userId, String product, double amount, String status) {
        this.id = id;
        this.userId = userId;
        this.product = product;
        this.amount = amount;
        this.status = status;
    }

    public Long getId() { return id; }
    public void setId(Long id) { this.id = id; }
    public String getUserId() { return userId; }
    public void setUserId(String userId) { this.userId = userId; }
    public String getProduct() { return product; }
    public void setProduct(String product) { this.product = product; }
    public double getAmount() { return amount; }
    public void setAmount(double amount) { this.amount = amount; }
    public String getStatus() { return status; }
    public void setStatus(String status) { this.status = status; }
}
