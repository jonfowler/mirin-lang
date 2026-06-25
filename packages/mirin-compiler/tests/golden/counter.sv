module counter #(parameter int bits) (
    input  logic clk,
    input  logic rstn,
    output logic [bits-1:0] result
);
    logic [bits-1:0] count;
    always_ff @(posedge clk) begin
        if (!rstn) begin
            count <= 0;
        end else begin
            count <= (count + 1);
        end
    end
    assign result = count;
    initial begin
        assert (1 < (1 << bits));
    end
    initial begin
        assert (0 < (1 << bits));
    end
endmodule
