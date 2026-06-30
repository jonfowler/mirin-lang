module counter (
    input  logic clk,
    input  logic rstn,
    output logic [7:0] result
);
    logic [7:0] count;
    always_ff @(posedge clk) begin
        if (!rstn) begin
            count <= 0;
        end else begin
            count <= (count + 1);
        end
    end
    assign result = count;
endmodule

module twice (
    input  logic [7:0] a,
    output logic [7:0] result
);
    logic [7:0] y;
    assign y = (a + a);
    assign result = y;
endmodule
