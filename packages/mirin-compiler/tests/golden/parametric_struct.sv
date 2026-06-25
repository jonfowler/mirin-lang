module pipeline (
    input  logic clk,
    input  logic rstn,
    input  logic b__valid,
    input  logic [7:0] b__data,
    output logic result__valid,
    output logic [7:0] result__data
);
    logic held__valid;
    always_ff @(posedge clk) begin
        if (!rstn) begin
            held__valid <= 1'b0;
        end else begin
            held__valid <= b__valid;
        end
    end
    logic [7:0] held__data;
    always_ff @(posedge clk) begin
        if (!rstn) begin
            held__data <= 0;
        end else begin
            held__data <= b__data;
        end
    end
    assign result__valid = held__valid;
    assign result__data = held__data;
endmodule

module pipeline_write (
    input  logic clk,
    input  logic rstn,
    input  logic w__valid,
    input  logic [7:0] w__data__addr,
    input  logic [7:0] w__data__data,
    output logic result__valid,
    output logic [7:0] result__data__addr,
    output logic [7:0] result__data__data
);
    logic held__valid;
    always_ff @(posedge clk) begin
        if (!rstn) begin
            held__valid <= 1'b0;
        end else begin
            held__valid <= w__valid;
        end
    end
    logic [7:0] held__data__addr;
    always_ff @(posedge clk) begin
        if (!rstn) begin
            held__data__addr <= 0;
        end else begin
            held__data__addr <= w__data__addr;
        end
    end
    logic [7:0] held__data__data;
    always_ff @(posedge clk) begin
        if (!rstn) begin
            held__data__data <= 0;
        end else begin
            held__data__data <= w__data__data;
        end
    end
    assign result__valid = held__valid;
    assign result__data__addr = held__data__addr;
    assign result__data__data = held__data__data;
endmodule
