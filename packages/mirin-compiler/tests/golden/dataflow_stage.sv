module stage (
    input  logic clk,
    input  logic up__valid,
    input  logic [7:0] up__data,
    output logic up__ready,
    input  logic rstn,
    output logic result__valid,
    output logic [7:0] result__data,
    input  logic result__ready
);
    logic vd;
    always_ff @(posedge clk) begin
        if (!rstn) begin
            vd <= 1'b0;
        end else begin
            vd <= up__valid;
        end
    end
    logic [7:0] dd;
    always_ff @(posedge clk) begin
        if (!rstn) begin
            dd <= 0;
        end else begin
            dd <= up__data;
        end
    end
    logic dn_ready;
    assign up__ready = dn_ready;
    assign dn_ready = result__ready;
    assign result__valid = vd;
    assign result__data = dd;
endmodule
